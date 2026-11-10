"""One dedicated Chromium profile. No cookie export and no browser extension."""
import queue
import threading
import time
from concurrent.futures import Future

ORIGIN = 'https://aichat.uva.nl'

FETCH = """async ({endpoint, payload}) => {
 const controller = new AbortController();
 const timer = setTimeout(() => controller.abort(), 120000);
 try {
   const path = endpoint === 'generic' ? '/api/v1/chat/generic' : '/api/v1/chat';
   const response = await fetch(path, {method:'POST', credentials:'same-origin', redirect:'error',
     headers:{'Content-Type':'application/json'}, body:JSON.stringify(payload), signal:controller.signal});
   if (response.status !== 200) return {status:response.status, events:[]};
   const events = [];
   const allowed = ['type','delta','toolCallId','toolName','input','finishReason'];
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


class BrowserSession:
    def __init__(self, profile):
        self.profile = profile
        self.state = 'starting'
        self.error = None
        self.jobs = queue.Queue()
        self.stop_event = threading.Event()
        self.thread = threading.Thread(target=self.run, name='uva-browser', daemon=True)

    def start(self):
        self.profile.mkdir(mode=0o700, parents=True, exist_ok=True)
        self.profile.chmod(0o700)
        self.thread.start()

    def status(self):
        return {'ready': self.state == 'ready', 'browser_state': self.state, 'browser_error': self.error}

    def request(self, payload, endpoint='generic'):
        if self.state != 'ready':
            raise ValueError('UvA login is required. Complete the login window, then retry.')
        future = Future()
        self.jobs.put((future, payload, endpoint))
        return future.result(timeout=145)

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
                        str(self.profile), channel='chromium', headless=headless, chromium_sandbox=True,
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
                                raise ValueError('UvA returned HTTP ' + str(result['status']))
                            future.set_result(result['events'])
                        except Exception:
                            if not future.done():
                                future.set_exception(ValueError('UvA browser request failed. Check login and network.'))
                    context.close()
                    context = None
                if context:
                    context.close()
        except Exception as error:
            self.state = 'error'
            # Avoid logging exception details that might contain browser/session data.
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
