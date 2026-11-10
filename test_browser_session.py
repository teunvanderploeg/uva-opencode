import unittest
from concurrent.futures import Future
from pathlib import Path
from unittest.mock import patch

from browser_session import BrowserSession
import make_release

class SessionTests(unittest.TestCase):
    def test_login_required_does_not_queue_model_requests(self):
        browser = BrowserSession(Path('/unused'))
        browser.state = 'login_required'
        with self.assertRaisesRegex(ValueError, 'login is required'):
            browser.request({})
        self.assertTrue(browser.jobs.empty())

    def test_ready_state_does_not_depend_on_extension_polling(self):
        browser = BrowserSession(Path('/unused'))
        browser.state = 'ready'
        self.assertTrue(browser.status()['ready'])
        browser.state = 'error'
        self.assertFalse(browser.status()['ready'])

    def test_background_worker_routes_the_payload_without_cookie_fields(self):
        browser = BrowserSession(Path('/unused'))
        browser.state = 'ready'
        with patch.object(Future, 'result', return_value=[{'type':'finish'}]):
            browser.request({'model':'gpt-6-luna'})
        _, payload, endpoint = browser.jobs.get_nowait()
        self.assertEqual(payload, {'model':'gpt-6-luna'})
        self.assertEqual(endpoint, 'generic')

    def test_release_allowlist_cannot_include_profiles_or_local_keys(self):
        self.assertTrue(all('/' not in path and not path.startswith('.') for path in make_release.FILES))
        self.assertNotIn('uva-opencode.user.js', make_release.FILES)
        self.assertNotIn('local-token', make_release.FILES)

if __name__ == '__main__':
    unittest.main()
