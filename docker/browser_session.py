"""One dedicated Chromium profile. No cookie export and no browser extension."""
import os
import queue
import socket
import sys
import threading
import time
import traceback
from concurrent.futures import Future

ORIGIN = 'https://aichat.uva.nl'
# A container often runs Chromium as a non-root user without the sandbox's user
# namespaces, so it sets UVA_CHROME_SANDBOX=0. A laptop keeps the sandbox on.
CHROME_SANDBOX = os.environ.get('UVA_CHROME_SANDBOX', '1') != '0'
# Pause before the browser is started again after an error (the login window comes back with it).
RETRY_SECONDS = 15
MAX_RETRY_SECONDS = 300

FETCH = """async ({endpoint, payload}) => {
 const controller = new AbortController();
 const timer = setTimeout(() => controller.abort(), 120000);
 try {
   const path = endpoint === 'generic' ? '/api/v1/chat/generic' : '/api/v1/chat';
   const response = await fetch(path, {method:'POST', credentials:'same-origin', redirect:'error',
     headers:{'Content-Type':'application/json'}, body:JSON.stringify(payload), signal:controller.signal});
   if (response.status !== 200) {
     const body = (await response.text()).slice(0, 400);
     return {status:response.status, events:[{type:'error', code:response.status, message:body}]};
   }
   const events = [];
   // Keep the error fields: UvA reports a refusal or backend failure as an error
   // event, and dropping its message leaves only "UvA returned an error event".
   const allowed = ['type','delta','toolCallId','toolName','input','finishReason',
     'message','error','code','detail','status'];
   for (const line of (await response.text()).split('\\n')) {
     if (!line.startsWith('data:') || line.slice(5).trim() === '[DONE]') continue;
     const event = JSON.parse(line.slice(5));
     events.push(Object.fromEntries(allowed.filter(k => k in event).map(k => [k,event[k]])));
   }
   return {status:response.status, events};
 } finally { clearTimeout(timer); }
}"""

AUTH = """async () => {
 try {
   const r = await fetch('/api/v1/auth/get-session', {credentials:'same-origin'});
   if (!r.ok) return false;
   const data = await r.json();
   return Boolean(data && data.user && data.session);
 } catch { return false; }
}"""


def chromium_alive(pid):
    # A restarted container reuses PIDs, so a live PID alone proves nothing: it must be Chromium.
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        pass
    try:
        with open('/proc/%d/cmdline' % pid, 'rb') as handle:
            return b'chrom' in handle.read().lower()
    except OSError:
        return True  # no /proc (macOS): the PID is alive, so assume it is the other browser


def clear_stale_profile_locks(profile):
    # Chromium's SingletonLock is "<hostname>-<pid>". A replaced container leaves one
    # behind with its old hostname, and Chromium then refuses the profile. One bridge
    # per profile is assumed: a lock from this host whose Chromium is still running
    # means another bridge is using it here.
    lock = profile / 'SingletonLock'
    if lock.is_symlink():
        host, _, pid = os.readlink(lock).rpartition('-')
        if host == socket.gethostname() and pid.isdigit() and chromium_alive(int(pid)):
            raise RuntimeError('another browser on this machine is using the profile (pid ' + pid + ')')
    for name in ('SingletonLock', 'SingletonCookie', 'SingletonSocket'):
        (profile / name).unlink(missing_ok=True)


def http_error(result):
    # Keep the status and the start of UvA's body: a bare status is not diagnosable.
    detail = (result.get('events') or [{}])[0].get('message', '')
    return ValueError('UvA returned HTTP %d: %s' % (result['status'], detail[:400]))


class BrowserSession:
    def __init__(self, profile):
        self.profile = profile
        self.state = 'starting'
        self.error = None
        self.jobs = queue.Queue()
        self.stop_event = threading.Event()
        self.thread = threading.Thread(target=self.supervise, name='uva-browser', daemon=True)

    def start(self):
        self.profile.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.profile.chmod(0o700)
        clear_stale_profile_locks(self.profile)
        self.thread.start()

    def status(self):
        return {'ready': self.state == 'ready', 'browser_state': self.state, 'browser_error': self.error}

    def request(self, payload, endpoint='generic'):
        if self.state != 'ready':
            raise ValueError('UvA login is required. Complete the login window, then retry.')
        future = Future()
        self.jobs.put((future, payload, endpoint))
        return future.result(timeout=145)

    def screenshot(self):
        # The logged-in browser is headless, so this is the only way to see its page.
        if self.state != 'ready':
            raise ValueError('UvA login is required. Complete the login window, then retry.')
        future = Future()
        self.jobs.put((future, {'__screenshot__': True}, 'screenshot'))
        return future.result(timeout=30)

    def supervise(self):
        # An error used to end the browser for good, leaving no login window. Start it again instead,
        # waiting longer after each error in a row (15 s, 30 s, ... up to 5 min).
        failures = 0
        while not self.stop_event.is_set():
            self.run()
            if self.state != 'error' or self.stop_event.is_set():
                break
            failures += 1
            self.stop_event.wait(min(RETRY_SECONDS * 2 ** (failures - 1), MAX_RETRY_SECONDS))

    def close(self):
        self.stop_event.set()
        self.thread.join(timeout=5)

    def run(self):
        context = None
        try:
            from playwright.sync_api import sync_playwright
            with sync_playwright() as playwright:
                def open_context(headless):
                    identity = self.profile.parent / 'browser-user-agent.txt'
                    options = {'user_agent': identity.read_text()} if identity.exists() else {}
                    context = playwright.chromium.launch_persistent_context(
                        str(self.profile), channel='chromium', headless=headless, chromium_sandbox=CHROME_SANDBOX,
                        accept_downloads=False, viewport={'width': 1100, 'height': 800}, **options)
                    page = context.pages[0] if context.pages else context.new_page()
                    page.goto(ORIGIN + '/chat', wait_until='domcontentloaded', timeout=60000)
                    return context, page

                def logged_in(page):
                    try:
                        return page.url.startswith(ORIGIN + '/') and page.evaluate(AUTH)
                    except Exception:
                        # Navigation back from Microsoft can invalidate an in-flight evaluation.
                        return False

                while not self.stop_event.is_set():
                    self.state = 'starting'
                    context, page = open_context(True)
                    if not logged_in(page):
                        context.close()
                        context, page = open_context(False)
                        self.state = 'login_required'
                        self.error = None
                        while not self.stop_event.is_set():
                            if not context.pages:
                                raise RuntimeError('Login window was closed before sign-in finished. Run uva-opencode again.')
                            if logged_in(page):
                                break
                            page.wait_for_timeout(1000)
                        if self.stop_event.is_set():
                            break
                        # UvA binds sessions to browser identity. Keep the actual login
                        # browser's user agent when running that same profile headlessly.
                        identity = self.profile.parent / 'browser-user-agent.txt'
                        identity.write_text(page.evaluate('navigator.userAgent'))
                        identity.chmod(0o600)
                        context.close()
                        context, page = open_context(True)
                        if not logged_in(page):
                            raise RuntimeError('UvA did not retain this login in the browser profile. Try --login again.')
                    self.state = 'ready'
                    last_check = time.monotonic()
                    while not self.stop_event.is_set():
                        try:
                            future, payload, endpoint = self.jobs.get(timeout=0.5)
                        except queue.Empty:
                            continue
                        if not future.set_running_or_notify_cancel():
                            continue
                        if isinstance(payload, dict) and payload.get('__screenshot__'):
                            try:
                                future.set_result(page.screenshot(type='png'))
                            except Exception as error:
                                future.set_exception(error)
                            continue
                        try:
                            if time.monotonic() - last_check > 30:
                                if not logged_in(page):
                                    self.state = 'login_required'
                                    future.set_exception(ValueError('UvA session expired. Complete the new login window, then retry.'))
                                    break
                                last_check = time.monotonic()
                            result = page.evaluate(FETCH, {'endpoint': endpoint, 'payload': payload})
                            if result['status'] in (401, 403):
                                self.state = 'login_required'
                                future.set_exception(ValueError('UvA session expired. Complete the new login window, then retry.'))
                                break
                            if result['status'] != 200:
                                raise http_error(result)
                            future.set_result(result['events'])
                        except Exception as error:
                            if not future.done():
                                future.set_exception(error if isinstance(error, ValueError)
                                                     else ValueError('UvA browser request failed. Check login and network.'))
                    context.close()
                    context = None
                if context:
                    context.close()
        except Exception as error:
            self.state = 'error'
            # Log where it failed, not the message: messages can carry browser or session data.
            frames = traceback.extract_tb(error.__traceback__)[-4:]
            try:
                print('browser error: ' + type(error).__name__ + ' at ' + ' -> '.join(
                    '%s:%s %s' % (frame.filename.rsplit('/', 1)[-1], frame.lineno, frame.name) for frame in frames),
                    file=sys.stderr, flush=True)
            except OSError:
                pass  # a closed stderr must not stop the error from being recorded
            self.error = ('Install the bundled browser with install.py.' if 'Executable doesn\'t exist' in str(error)
                          else str(error) if isinstance(error, RuntimeError)
                          else 'Browser operation failed: ' + type(error).__name__ + '. Run uva-opencode --login to retry.')
        finally:
            if self.state != 'error':
                self.state = 'stopped'
            while not self.jobs.empty():
                future, _, _ = self.jobs.get_nowait()
                if not future.done():
                    future.set_exception(ValueError('The browser helper stopped.'))
