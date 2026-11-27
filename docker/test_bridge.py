import io
import json
import unittest
from unittest.mock import MagicMock, patch

import bridge
import browser_session


class LocalAccess(unittest.TestCase):
    def accepted(self, path, key, origin=None, host='127.0.0.1:8765'):
        handler = object.__new__(bridge.Handler)
        handler.path = path
        handler.headers = {'Host': host, 'Authorization': 'Bearer ' + key}
        if origin:
            handler.headers['Origin'] = origin
        with patch.object(bridge, 'TOKEN', 'api-key'), patch.object(bridge, 'ALLOWED_HOSTS', {'127.0.0.1:8765'}):
            return handler.authorized()

    def test_key_is_required_for_the_model_and_shutdown(self):
        self.assertTrue(self.accepted('/v1/chat/completions', 'api-key'))
        self.assertTrue(self.accepted('/shutdown', 'api-key'))
        self.assertFalse(self.accepted('/v1/chat/completions', 'wrong'))

    def test_any_browser_origin_is_refused(self):
        self.assertFalse(self.accepted('/health', 'api-key', origin='https://aichat.uva.nl'))
        self.assertFalse(self.accepted('/v1/chat/completions', 'api-key', origin='https://example.com'))

    def test_other_hosts_are_rejected(self):
        self.assertFalse(self.accepted('/health', 'api-key', host='example.com:8765'))


class CustomHost(unittest.TestCase):
    # The Host allowlist comes from UVA_ALLOWED_HOSTS, so a deployment can reach the
    # bridge by its own host name, not just 127.0.0.1.
    def allowed(self, host, auth='Bearer key', origin=None):
        handler = object.__new__(bridge.Handler)
        handler.path = '/v1/models'
        handler.headers = {'Host': host, 'Authorization': auth}
        if origin:
            handler.headers['Origin'] = origin
        with patch.object(bridge, 'ALLOWED_HOSTS', {'bridge.local:8765'}), patch.object(bridge, 'TOKEN', 'key'):
            return handler.authorized()

    def test_allowlisted_host_with_key_is_allowed(self):
        self.assertTrue(self.allowed('bridge.local:8765'))

    def test_other_host_is_refused(self):
        self.assertFalse(self.allowed('evil.example:8765'))

    def test_wrong_or_missing_key_is_refused(self):
        self.assertFalse(self.allowed('bridge.local:8765', auth='Bearer nope'))
        self.assertFalse(self.allowed('bridge.local:8765', auth=''))

    def test_browser_origin_is_refused(self):
        self.assertFalse(self.allowed('bridge.local:8765', origin='https://example.com'))


class Screenshot(unittest.TestCase):
    def test_screenshot_returns_png(self):
        handler = object.__new__(bridge.Handler)
        handler.path = '/screenshot'
        handler.headers = {'Host': '127.0.0.1:8765', 'Authorization': 'Bearer key'}
        handler.wfile = io.BytesIO()
        handler.send_response = MagicMock()
        handler.send_header = MagicMock()
        handler.end_headers = MagicMock()

        class FakeBrowser:
            def screenshot(self):
                return b'\x89PNG'

        with patch.object(bridge, 'TOKEN', 'key'), patch.object(bridge, 'ALLOWED_HOSTS', {'127.0.0.1:8765'}), \
                patch.object(bridge, 'BROWSER', FakeBrowser()):
            handler.do_GET()
        self.assertEqual(handler.wfile.getvalue(), b'\x89PNG')


class ModelCatalog(unittest.TestCase):
    # /v1/models reads opencode.example.json from the image; a missing file crashed the handler.
    def test_catalog_file_loads_with_models(self):
        self.assertIn('gpt-6.1-sol', bridge.model_catalog())

    def test_catalog_offers_prompt_mode_tools(self):
        uva = json.loads((bridge.ROOT / 'opencode.example.json').read_text())['providers']['uva']
        self.assertEqual(uva['body'], {'uva_tool_mode': 'prompt'})
        self.assertTrue(all(m['capabilities']['tools'] is True for m in uva['models'].values()))


class Server(unittest.TestCase):
    def test_stream_sends_content_chunk_then_done(self):
        handler = object.__new__(bridge.Handler)
        handler.path = '/v1/chat/completions'
        data = json.dumps({'messages': [{'role': 'user', 'content': 'hi'}], 'model': 'gpt-6.1-sol',
                           'tools': [{'type': 'function', 'function': {'name': 'read', 'parameters': {}}}],
                           'stream': True}).encode()
        handler.headers = {'Content-Length': str(len(data))}
        handler.rfile = io.BytesIO(data)
        handler.wfile = io.BytesIO()
        handler.send_response = MagicMock()
        handler.send_header = MagicMock()
        handler.end_headers = MagicMock()
        reply = json.dumps({'content': 'hello', 'tool_calls': []})
        with patch.object(handler, 'authorized', return_value=True), \
                patch.object(bridge, 'upstream', return_value=[{'type': 'text-delta', 'delta': reply}]):
            handler.do_POST()
        lines = handler.wfile.getvalue().decode().split('\n\n')
        self.assertEqual(lines[-2:], ['data: [DONE]', ''])
        chunk = json.loads(lines[0][len('data: '):])
        self.assertEqual(chunk['choices'][0]['delta']['content'], 'hello')
        self.assertEqual(json.loads(lines[1][len('data: '):])['choices'][0]['finish_reason'], 'stop')


class ImageFiles(unittest.TestCase):
    # Every file the bridge reads at runtime must be copied into the image.
    def test_dockerfile_copies_runtime_files(self):
        import pathlib
        here = pathlib.Path(bridge.__file__).parent
        copied = (here / 'Dockerfile').read_text()
        for name in ('bridge.py', 'browser_session.py', 'runtime_paths.py', 'opencode.example.json', 'entrypoint.sh'):
            self.assertIn(name, copied)

    def test_entrypoint_starts_the_bridge_without_removed_flags(self):
        import pathlib
        entry = (pathlib.Path(bridge.__file__).parent / 'entrypoint.sh').read_text()
        self.assertIn('exec python /app/bridge.py\n', entry)
        self.assertNotIn('--tool-mode', entry)


class StaleProfileLock(unittest.TestCase):
    # A replaced container leaves Chromium's SingletonLock behind; the bridge must clear it before launching.
    def test_stale_singleton_links_are_removed(self):
        import pathlib
        import tempfile
        import browser_session
        with tempfile.TemporaryDirectory() as tmp:
            profile = pathlib.Path(tmp)
            (profile / 'SingletonLock').symlink_to('uva-bridge-old-host-164')
            (profile / 'SingletonCookie').symlink_to('123')
            (profile / 'Default').mkdir()
            browser_session.clear_stale_profile_locks(profile)
            self.assertFalse((profile / 'SingletonLock').is_symlink())
            self.assertFalse((profile / 'SingletonCookie').is_symlink())
            self.assertTrue((profile / 'Default').is_dir())

    def test_live_lock_on_this_host_is_kept(self):
        import os
        import pathlib
        import socket
        import tempfile
        import browser_session
        with tempfile.TemporaryDirectory() as tmp:
            profile = pathlib.Path(tmp)
            (profile / 'SingletonLock').symlink_to(socket.gethostname() + '-' + str(os.getpid()))
            with patch.object(browser_session, 'chromium_alive', return_value=True), \
                    self.assertRaisesRegex(RuntimeError, 'another browser'):
                browser_session.clear_stale_profile_locks(profile)
            self.assertTrue((profile / 'SingletonLock').is_symlink())

    def test_reused_pid_that_is_not_chromium_is_cleared(self):
        import os
        import pathlib
        import socket
        import tempfile
        import browser_session
        with tempfile.TemporaryDirectory() as tmp:
            profile = pathlib.Path(tmp)
            (profile / 'SingletonLock').symlink_to(socket.gethostname() + '-' + str(os.getpid()))
            with patch.object(browser_session, 'chromium_alive', return_value=False):
                browser_session.clear_stale_profile_locks(profile)
            self.assertFalse((profile / 'SingletonLock').is_symlink())

    def test_no_lock_files_is_fine(self):
        import pathlib
        import tempfile
        import browser_session
        with tempfile.TemporaryDirectory() as tmp:
            browser_session.clear_stale_profile_locks(pathlib.Path(tmp))


class Supervision(unittest.TestCase):
    # An error must start the browser again (and its login window), not end it for good.
    def test_error_starts_the_browser_again(self):
        import pathlib
        import tempfile
        import browser_session
        with tempfile.TemporaryDirectory() as tmp:
            session = browser_session.BrowserSession(pathlib.Path(tmp))
            calls = []

            def fake_run():
                calls.append(1)
                session.state = 'error' if len(calls) == 1 else 'stopped'
                if len(calls) == 2:
                    session.stop_event.set()

            with patch.object(session, 'run', side_effect=fake_run), \
                    patch.object(browser_session, 'RETRY_SECONDS', 0):
                session.supervise()
            self.assertEqual(len(calls), 2)

    def test_login_required_is_not_restarted(self):
        import pathlib
        import tempfile
        import browser_session
        with tempfile.TemporaryDirectory() as tmp:
            session = browser_session.BrowserSession(pathlib.Path(tmp))
            calls = []

            def fake_run():
                calls.append(1)
                session.state = 'login_required'

            with patch.object(session, 'run', side_effect=fake_run):
                session.supervise()
            self.assertEqual(len(calls), 1)

    def test_close_during_the_wait_returns_promptly(self):
        import pathlib
        import tempfile
        import threading
        import time
        import browser_session
        with tempfile.TemporaryDirectory() as tmp:
            session = browser_session.BrowserSession(pathlib.Path(tmp))

            def fake_run():
                session.state = 'error'
                threading.Timer(0.1, session.stop_event.set).start()

            started = time.monotonic()
            with patch.object(session, 'run', side_effect=fake_run), \
                    patch.object(browser_session, 'RETRY_SECONDS', 60):
                session.supervise()
            self.assertLess(time.monotonic() - started, 5)


class UvaErrorDetail(unittest.TestCase):
    def test_error_event_keeps_message_and_code(self):
        with self.assertRaises(ValueError) as ctx:
            bridge.text_of([{'type': 'error', 'code': 403, 'message': 'request refused'}])
        self.assertIn('403', str(ctx.exception))
        self.assertIn('request refused', str(ctx.exception))

    def test_http_error_keeps_the_status_and_body(self):
        err = browser_session.http_error({'status': 500, 'events': [
            {'type': 'error', 'code': 500, 'message': 'upstream said X'}]})
        self.assertIn('500', str(err))
        self.assertIn('upstream said X', str(err))


class UpstreamRetry(unittest.TestCase):
    def test_bare_error_is_retried_once(self):
        browser = MagicMock()
        browser.request.side_effect = [[{'type': 'error'}], [{'type': 'text-delta', 'delta': 'ok'}]]
        with patch.object(bridge, 'BROWSER', browser), patch.object(bridge, 'UPSTREAM_RETRY_SECONDS', 5), \
                patch.object(bridge.time, 'sleep') as sleep:
            events = bridge.upstream({'model': 'm'})
        self.assertEqual(browser.request.call_count, 2)
        sleep.assert_called_once_with(5)
        self.assertEqual(events, [{'type': 'text-delta', 'delta': 'ok'}])

    def test_only_a_bare_error_is_retried(self):
        self.assertTrue(bridge.bare_error([{'type': 'error', 'message': '', 'code': '', 'detail': ''}]))
        self.assertFalse(bridge.bare_error([{'type': 'error', 'code': 403}]))
        self.assertFalse(bridge.bare_error([{'type': 'text-delta', 'delta': 'ok'}]))

    def test_named_error_is_not_retried(self):
        browser = MagicMock()
        browser.request.return_value = [{'type': 'error', 'message': 'Session expired.'}]
        with patch.object(bridge, 'BROWSER', browser), patch.object(bridge.time, 'sleep') as sleep:
            bridge.upstream({'model': 'm'})
        self.assertEqual(browser.request.call_count, 1)
        sleep.assert_not_called()

    def test_second_bare_error_is_surfaced(self):
        browser = MagicMock()
        browser.request.return_value = [{'type': 'error'}]
        with patch.object(bridge, 'BROWSER', browser), patch.object(bridge.time, 'sleep'):
            events = bridge.upstream({'model': 'm'})
        self.assertEqual(browser.request.call_count, 2)
        self.assertTrue(bridge.bare_error(events))

    def test_retry_can_be_disabled(self):
        browser = MagicMock()
        browser.request.return_value = [{'type': 'error'}]
        with patch.object(bridge, 'BROWSER', browser), patch.object(bridge, 'UPSTREAM_ATTEMPTS', 1), \
                patch.object(bridge.time, 'sleep'):
            bridge.upstream({'model': 'm'})
        self.assertEqual(browser.request.call_count, 1)


class BrowserEventFilter(unittest.TestCase):
    def test_usage_fields_survive_the_filter(self):
        # The browser adapter filters SSE events before they reach the handler;
        # usage/totalUsage must pass through so the bridge reports real token counts
        # instead of estimates.
        import browser_session
        self.assertIn("'usage'", browser_session.FETCH)
        self.assertIn("'totalUsage'", browser_session.FETCH)
