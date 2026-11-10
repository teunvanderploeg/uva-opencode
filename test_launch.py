import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import launch


class Startup(unittest.TestCase):
    def test_ready_bridge_is_reused_without_starting_a_process(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(launch, 'STATE', Path(folder)), \
                patch.object(launch, 'health', return_value={'ready': True}), \
                patch.object(launch.subprocess, 'Popen') as start:
            self.assertTrue(launch.ensure_bridge()['ready'])
            start.assert_not_called()

    def test_missing_bridge_is_started_once(self):
        process = Mock()
        process.poll.return_value = None
        with tempfile.TemporaryDirectory() as folder, patch.object(launch, 'STATE', Path(folder)), \
                patch.object(launch, 'FROZEN', True), \
                patch.object(launch, 'health', side_effect=[None, None, {'ready': False}]), \
                patch.object(launch.subprocess, 'Popen', return_value=process) as start, \
                patch.object(launch.time, 'sleep'):
            self.assertFalse(launch.ensure_bridge()['ready'])
            self.assertEqual(start.call_count, 1)
            self.assertIn('--internal-bridge', start.call_args.args[0])
            if os.name == 'nt':
                self.assertEqual(start.call_args.kwargs['creationflags'], subprocess.CREATE_NEW_PROCESS_GROUP)
            else:
                self.assertTrue(start.call_args.kwargs['start_new_session'])
            process.terminate.assert_not_called()

    def test_failed_start_does_not_launch_opencode(self):
        process = Mock()
        process.poll.return_value = 1
        with tempfile.TemporaryDirectory() as folder, patch.object(launch, 'STATE', Path(folder)), \
                patch.object(launch, 'FROZEN', True), \
                patch.object(launch, 'health', return_value=None), \
                patch.object(launch.subprocess, 'Popen', return_value=process):
            with self.assertRaisesRegex(RuntimeError, 'could not start'):
                launch.ensure_bridge()

    def test_unrelated_service_on_port_is_rejected(self):
        with patch.object(launch, 'api', return_value={'service': 'another-app', 'protocol': 2}):
            with self.assertRaises(RuntimeError):
                launch.health()

    def test_ready_connection_does_not_open_another_browser_tab(self):
        with patch('sys.argv', ['launch.py', '--check']), \
                patch.object(launch, 'ensure_bridge', return_value={'ready': True}), \
                patch.object(launch, 'wait_for_relay') as open_browser:
            self.assertEqual(launch.main(), 0)
            open_browser.assert_not_called()


if __name__ == '__main__':
    unittest.main()
