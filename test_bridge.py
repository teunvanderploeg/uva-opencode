import json
import unittest
from unittest.mock import patch

import bridge


class TransportValidation(unittest.TestCase):
    def setUp(self):
        self.body = {'messages': [{'role': 'user', 'content': 'test'}], 'uva_tool_mode': 'prompt',
                     'tools': [{'type': 'function', 'function': {'name': 'read', 'parameters': {}}}]}

    def respond(self, value, **changes):
        value = dict({'content': None}, **value)
        body = dict(self.body, **changes)
        events = [{'type': 'text-delta', 'delta': json.dumps(value)}]
        with patch.object(bridge, 'upstream', return_value=events):
            return bridge.completion(body)

    def test_unknown_tool_is_rejected(self):
        with self.assertRaisesRegex(ValueError, 'unknown tool'):
            self.respond({'content': None, 'tool_calls': [{'name': 'shell', 'arguments': {}}]})

    def test_string_arguments_are_rejected(self):
        with self.assertRaisesRegex(bridge.TransportError, 'one repair attempt'):
            self.respond({'tool_calls': [{'name': 'read', 'arguments': 'not an object'}]})

    def test_formatting_error_returns_non_retryable_http_status(self):
        import io
        handler = object.__new__(bridge.Handler)
        handler.path = '/v1/chat/completions'
        data = json.dumps(self.body).encode()
        handler.headers = {'Content-Length': str(len(data))}
        handler.rfile = io.BytesIO(data)
        with patch.object(handler, 'authorized', return_value=True), patch.object(handler, 'reply') as reply, \
                patch.object(bridge, 'upstream', return_value=[{'type': 'text-delta', 'delta': '<html>bad format</html>'}]):
            handler.do_POST()
        self.assertEqual(reply.call_args.args[0], 422)
        self.assertEqual(reply.call_args.args[1]['error']['type'], 'uva_transport_error')

    def test_tool_choice_none_is_enforced(self):
        with self.assertRaisesRegex(ValueError, 'tool_choice was none'):
            self.respond({'tool_calls': [{'name': 'read', 'arguments': {}}]}, tool_choice='none')

    def test_required_tool_is_enforced(self):
        with self.assertRaisesRegex(ValueError, 'Required client tool'):
            self.respond({'content': 'I read it', 'tool_calls': []}, tool_choice='required')

    def test_selected_tool_is_enforced(self):
        with self.assertRaisesRegex(ValueError, 'selected function'):
            self.respond({'content': 'done', 'tool_calls': []},
                         tool_choice={'type': 'function', 'function': {'name': 'read'}})

    def test_malformed_transport_is_rejected(self):
        with patch.object(bridge, 'upstream', return_value=[{'type': 'text-delta', 'delta': 'plain text'}]) as request:
            with self.assertRaisesRegex(bridge.TransportError, 'one repair attempt'):
                bridge.completion(self.body)
            self.assertEqual(request.call_count, 2)

    def test_json_fence_with_explanation_is_accepted_without_repair(self):
        obj = {'content': None, 'tool_calls': [{'name': 'read', 'arguments': {'path': 'index.html'}}]}
        text = 'I will inspect the file first.\n```json\n' + json.dumps(obj) + '\n```'
        with patch.object(bridge, 'upstream', return_value=[{'type': 'text-delta', 'delta': text}]) as request:
            result = bridge.completion(self.body)
            self.assertEqual(result['choices'][0]['message']['tool_calls'][0]['function']['name'], 'read')
            self.assertEqual(request.call_count, 1)

    def test_html_response_is_repaired_before_any_client_tool_executes(self):
        fixed = {'content': None, 'tool_calls': [{'name': 'read', 'arguments': {'path': 'index.html'}}]}
        with patch.object(bridge, 'upstream', side_effect=[
                [{'type': 'text-delta', 'delta': '<!doctype html><html>plain HTML</html>'}],
                [{'type': 'text-delta', 'delta': json.dumps(fixed)}],
        ]) as request:
            result = bridge.completion(self.body)
            self.assertEqual(len(result['choices'][0]['message']['tool_calls']), 1)
            self.assertEqual(request.call_count, 2)
            self.assertIn('JSON envelope is mandatory', request.call_args_list[0].args[0]['systemPrompt'])
            self.assertTrue(all(m['role'] != 'system' for m in request.call_args_list[0].args[0]['messages']))
            self.assertIn('No tools from your previous response have executed',
                          request.call_args_list[1].args[0]['messages'][-1]['parts'][0]['text'])

    def test_html_json_strings_preserve_quotes_and_newlines(self):
        html = '<html>\n<style>body { color: red; }</style>\n<div class="hero">UvA</div></html>'
        obj = {'content': None, 'tool_calls': [{'name': 'write', 'arguments': {'content': html}}]}
        self.assertEqual(bridge.transport_object(json.dumps(obj)), obj)

    def test_generated_html_is_not_scanned_for_embedded_tool_instructions(self):
        text = '<html><script>{"content":null,"tool_calls":[{"name":"read","arguments":{}}]}</script></html>'
        with self.assertRaises(bridge.TransportError):
            bridge.transport_object(text)

    def test_truncated_output_is_not_retried_as_a_formatting_error(self):
        with patch.object(bridge, 'upstream', return_value=[
                {'type': 'text-delta', 'delta': '{"content":'}, {'type': 'finish', 'finishReason': 'length'}]) as request:
            with self.assertRaisesRegex(bridge.TransportError, 'truncated'):
                bridge.completion(self.body)
            self.assertEqual(request.call_count, 1)

    def test_empty_object_does_not_silently_become_a_successful_answer(self):
        with self.assertRaises(bridge.TransportError):
            bridge.transport_object('{}')

    def test_native_tool_history_is_not_silently_dropped(self):
        with self.assertRaisesRegex(ValueError, 'Native tool history'):
            bridge.completion({'uva_tool_mode': 'native', 'messages': [
                {'role': 'tool', 'tool_call_id': 'x', 'content': 'secret value'}]})


class LocalAccess(unittest.TestCase):
    def accepted(self, path, key, origin=None, host='127.0.0.1:8765'):
        handler = object.__new__(bridge.Handler)
        handler.path = path
        handler.headers = {'Host': host, 'Authorization': 'Bearer ' + key}
        if origin:
            handler.headers['Origin'] = origin
        with patch.object(bridge, 'TOKEN', 'api-key'), patch.object(bridge, 'RELAY_TOKEN', 'relay-key'):
            return handler.authorized()

    def test_browser_relay_key_cannot_call_model_or_shutdown_api(self):
        self.assertFalse(self.accepted('/v1/chat/completions', 'relay-key'))
        self.assertFalse(self.accepted('/shutdown', 'relay-key'))
        self.assertTrue(self.accepted('/relay/next', 'relay-key', 'https://aichat.uva.nl'))

    def test_api_key_does_not_impersonate_the_browser_relay(self):
        self.assertTrue(self.accepted('/health', 'api-key'))
        self.assertFalse(self.accepted('/relay/result', 'api-key'))

    def test_other_websites_and_hosts_are_rejected(self):
        self.assertFalse(self.accepted('/relay/next', 'relay-key', 'https://example.com'))
        self.assertFalse(self.accepted('/health', 'api-key', host='example.com:8765'))


if __name__ == '__main__':
    unittest.main()
