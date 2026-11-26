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

    def test_rejected_reply_start_is_logged_before_repair(self):
        import io
        from contextlib import redirect_stderr
        payload = {'messages': [{'role': 'user', 'parts': [{'type': 'text', 'text': 'x'}]}], 'model': 'm'}
        log = io.StringIO()
        with patch.object(bridge, 'upstream', side_effect=[
                [{'type': 'text-delta', 'delta': 'PLAN. prose instead of the envelope'}],
                [{'type': 'text-delta', 'delta': json.dumps({'content': 'ok', 'tool_calls': []})}],
        ]), redirect_stderr(log):
            bridge.prompt_response(payload)
        self.assertIn("chat reply rejected: 'PLAN. prose instead of the envelope' (events: text-delta)",
                      log.getvalue())

    def test_empty_reply_logs_the_event_types_it_received(self):
        import io
        from contextlib import redirect_stderr
        payload = {'messages': [{'role': 'user', 'parts': [{'type': 'text', 'text': 'x'}]}], 'model': 'm'}
        log = io.StringIO()
        with patch.object(bridge, 'upstream', side_effect=[
                [{'type': 'start'}, {'type': 'finish', 'finishReason': 'stop'}],
                [{'type': 'text-delta', 'delta': json.dumps({'content': 'ok', 'tool_calls': []})}],
        ]), redirect_stderr(log):
            bridge.prompt_response(payload)
        self.assertIn("chat reply rejected: '' (events: finish,start)", log.getvalue())

    def test_repair_echoes_only_the_start_of_a_long_reply(self):
        with patch.object(bridge, 'upstream', side_effect=[
                [{'type': 'text-delta', 'delta': 'x' * 20000}],
                [{'type': 'text-delta', 'delta': json.dumps({'content': 'done', 'tool_calls': []})}],
        ]) as request:
            bridge.completion(self.body)
        echo = request.call_args_list[1].args[0]['messages'][-2]['parts'][0]['text']
        self.assertLess(len(echo), 4200)
        self.assertIn('more characters of your previous reply omitted', echo)

    def test_valid_envelope_with_trailing_prose_is_repaired(self):
        obj = {'content': 'done', 'tool_calls': []}
        with patch.object(bridge, 'upstream', side_effect=[
                [{'type': 'text-delta', 'delta': json.dumps(obj) + '\nDone.'}],
                [{'type': 'text-delta', 'delta': json.dumps(obj)}]]) as request:
            bridge.completion(self.body)
        self.assertEqual(request.call_count, 2)

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
            self.assertIn('so no tool executed',
                          request.call_args_list[1].args[0]['messages'][-1]['parts'][0]['text'])

    def test_html_json_strings_preserve_quotes_and_newlines(self):
        html = '<html>\n<style>body { color: red; }</style>\n<div class="hero">UvA</div></html>'
        obj = {'content': None, 'tool_calls': [{'name': 'write', 'arguments': {'content': html}}]}
        self.assertEqual(bridge.transport_object(json.dumps(obj)), obj)

    def test_generated_html_is_not_scanned_for_embedded_tool_instructions(self):
        text = '<html><script>{"content":null,"tool_calls":[{"name":"read","arguments":{}}]}</script></html>'
        with self.assertRaises(bridge.TransportError):
            bridge.transport_object(text)

    def test_truncated_output_is_repaired_once_with_a_shorter_request(self):
        fixed = json.dumps({'content': 'done', 'tool_calls': []})
        with patch.object(bridge, 'upstream', side_effect=[
                [{'type': 'text-delta', 'delta': '{"content":'},
                 {'type': 'finish', 'finishReason': 'length'}],
                [{'type': 'text-delta', 'delta': fixed}]]) as request:
            result = bridge.completion(self.body)
        self.assertEqual(request.call_count, 2)
        repair = request.call_args_list[1].args[0]['messages'][-1]['parts'][0]['text']
        self.assertIn('valid JSON transport object', repair)
        self.assertIn('keep it shorter', repair)
        self.assertEqual(result['choices'][0]['message']['content'], 'done')

    def test_a_second_truncated_reply_fails_closed(self):
        events = [{'type': 'text-delta', 'delta': '{"content":'},
                  {'type': 'finish', 'finishReason': 'length'}]
        with patch.object(bridge, 'upstream', return_value=events) as request:
            with self.assertRaisesRegex(bridge.TransportError, 'one repair attempt'):
                bridge.completion(self.body)
        self.assertEqual(request.call_count, 2)

    def test_empty_object_does_not_silently_become_a_successful_answer(self):
        with self.assertRaises(bridge.TransportError):
            bridge.transport_object('{}')

    def test_native_tool_history_is_not_silently_dropped(self):
        with self.assertRaisesRegex(ValueError, 'Native tool history'):
            bridge.completion({'uva_tool_mode': 'native', 'messages': [
                {'role': 'tool', 'tool_call_id': 'x', 'content': 'secret value'}]})

    def test_compact_history_preserves_code_and_tool_links(self):
        code = 'const greeting = "UvA 🦀";\n' * 100
        arguments = {'path': 'main.js', 'content': code}
        messages = [
            {'role': 'system', 'content': 'Keep these instructions.'},
            {'role': 'user', 'content': [{'type': 'text', 'text': 'First'}, {'type': 'text', 'text': 'Second'}]},
            {'role': 'assistant', 'content': None, 'tool_calls': [
                {'id': 'original', 'type': 'function', 'function': {'name': 'write', 'arguments': json.dumps(arguments)}},
                {'id': 'broken', 'type': 'function', 'function': {'name': 'read', 'arguments': '{broken'}}]},
            {'role': 'tool', 'tool_call_id': 'original', 'content': 'Written'}]
        original = json.dumps(messages)
        text = bridge.shim_messages({'messages': messages})[0][0]['parts'][0]['text'].split('\n', 1)[1]
        compact = json.loads(text)
        self.assertLess(len(text), len(original))
        self.assertEqual(len(compact), 3)
        self.assertEqual(compact[0]['content'], 'First\nSecond')
        self.assertEqual(compact[1]['tool_calls'][0]['function']['arguments'], arguments)
        self.assertEqual(compact[1]['tool_calls'][0]['id'], 'original')
        self.assertEqual(compact[1]['tool_calls'][1]['function']['arguments'], '{broken')
        self.assertEqual(compact[2], messages[3])
        self.assertTrue(all(m['role'] != 'system' for m in compact))
        self.assertIsInstance(messages[2]['tool_calls'][0]['function']['arguments'], str)

    def test_long_tool_result_is_trimmed_to_head_and_tail(self):
        body = {'messages': [{'role': 'user', 'content': 'build it'},
                             {'role': 'tool', 'tool_call_id': 'c',
                              'content': 'A' * 5000 + 'Z' * 5000}]}
        messages, note = bridge.capped_conversation(bridge.normalized_messages(body))
        self.assertEqual(note['trimmed_tool_results'], 1)
        self.assertEqual(note['dropped_messages'], 0)
        text = messages[-1]['content']
        self.assertLess(len(text), 9000)
        self.assertIn('characters of this tool result omitted', text)
        self.assertTrue(text.startswith('A') and text.endswith('Z'))

    def test_old_turns_are_dropped_and_the_task_is_kept(self):
        messages = [{'role': 'user', 'content': 'TASK'}]
        for n in range(40):
            messages.append({'role': 'user', 'content': 'step %d ' % n + 'x' * 300})
        kept, note = bridge.capped_conversation(messages, budget=1200)
        self.assertEqual(kept[0]['content'], 'TASK')
        self.assertEqual(kept[-1], messages[-1])
        self.assertGreater(note['dropped_messages'], 5)
        self.assertGreater(note['dropped_tokens'], 0)
        self.assertLess(sum(bridge.estimate_tokens(bridge.compact_json(m)) for m in kept), 1600)

    def test_a_cut_that_lands_on_a_tool_result_keeps_its_call(self):
        call = {'role': 'assistant', 'content': None, 'tool_calls': [
            {'id': 'c', 'type': 'function', 'function': {'name': 'read', 'arguments': '{}'}}]}
        messages = [{'role': 'user', 'content': 'TASK'},
                    {'role': 'user', 'content': 'x' * 400}, call,
                    {'role': 'tool', 'tool_call_id': 'c', 'content': 'y' * 400}]
        kept, _ = bridge.capped_conversation(messages, budget=60)
        self.assertEqual(kept[0]['content'], 'TASK')
        self.assertEqual([m['role'] for m in kept[1:]], ['assistant', 'tool'])

    def test_many_results_in_one_turn_are_retrimmed_to_the_budget(self):
        call = {'role': 'assistant', 'content': None, 'tool_calls': [
            {'id': 'c', 'type': 'function', 'function': {'name': 'read', 'arguments': '{}'}}]}
        messages = [{'role': 'user', 'content': 'TASK'}, call]
        messages += [{'role': 'tool', 'tool_call_id': 'c', 'content': 'z' * 9000} for _ in range(12)]
        kept, note = bridge.capped_conversation(messages, budget=16000)
        self.assertEqual(note['dropped_messages'], 0)
        self.assertLessEqual(note['kept_tokens'], 16000)
        self.assertFalse(note['over_budget'])
        self.assertGreaterEqual(note['trimmed_tool_results'], 12)
        self.assertEqual([m['role'] for m in kept], ['user', 'assistant'] + ['tool'] * 12)

    def test_an_oversized_task_is_sent_whole_and_reported(self):
        messages = [{'role': 'user', 'content': 'x' * 90000}]
        kept, note = bridge.capped_conversation(messages, budget=1000)
        self.assertEqual(kept, messages)
        self.assertTrue(note['over_budget'])
        self.assertEqual(note['dropped_messages'], 0)

    def test_the_caps_can_be_disabled(self):
        messages = [{'role': 'user', 'content': 'TASK'},
                    {'role': 'tool', 'tool_call_id': 'c', 'content': 'x' * 40000}]
        with patch.object(bridge, 'MAX_CONVERSATION_TOKENS', 0), patch.object(bridge, 'MAX_TOOL_RESULT_CHARS', 0):
            kept, note = bridge.capped_conversation(messages)
        self.assertEqual(kept, messages)
        self.assertEqual(note['trimmed_tool_results'], 0)

    def test_usage_estimates_and_provider_counts(self):
        result = self.respond({'content': 'hello', 'tool_calls': []})
        usage = result['usage']
        self.assertGreater(usage['prompt_tokens'], 0)
        self.assertGreater(usage['completion_tokens'], 0)
        self.assertEqual(usage['total_tokens'], usage['prompt_tokens'] + usage['completion_tokens'])
        for key in ('usage', 'totalUsage'):
            usage, source = bridge.response_usage({}, [{'type': 'finish', key: {'inputTokens': 1234, 'outputTokens': 56}}], '')
            self.assertEqual(source, 'upstream')
            self.assertEqual(usage, {'prompt_tokens': 1234, 'completion_tokens': 56, 'total_tokens': 1290})
        usage, source = bridge.response_usage({}, [{'usage': {'inputTokens': -1, 'outputTokens': 2}}], 'hello')
        self.assertEqual(source, 'estimated')

    def test_prompt_images_are_rejected(self):
        with self.assertRaisesRegex(ValueError, 'text messages only'):
            bridge.shim_messages({'messages': [{'role': 'user', 'content': [{'type': 'image_url'}]}]})

    def test_breakdown_tracks_results_without_logging_contents(self):
        body = dict(self.body, messages=[
            {'role': 'system', 'content': 'PRIVATE_SYSTEM'},
            {'role': 'developer', 'content': 'PRIVATE_POLICY'},
            {'role': 'user', 'content': 'PRIVATE_TASK'},
            {'role': 'tool', 'tool_call_id': 'original', 'content': 'PRIVATE_RESULT'}])
        before = bridge.input_breakdown(body, 'prompt')
        self.assertEqual(len(before), 5)
        self.assertEqual(before['client_instructions'], 0)
        self.assertTrue(all(value > 0 for key, value in before.items() if key != 'client_instructions'))
        body['messages'][-1]['content'] = '🦀' * 3000
        after = bridge.input_breakdown(body, 'prompt')
        self.assertGreater(after['tool_results'], before['tool_results'])
        self.assertEqual({k: v for k, v in before.items() if k != 'tool_results'},
                         {k: v for k, v in after.items() if k != 'tool_results'})
        valid = json.dumps({'content': 'done', 'tool_calls': []})
        with patch.object(bridge, 'upstream', side_effect=[
                [{'type': 'text-delta', 'delta': 'invalid'}],
                [{'type': 'text-delta', 'delta': valid}]]):
            bridge.completion(body)
        stats = bridge.STATS[-1]
        self.assertEqual(stats['upstream_requests'], 2)
        self.assertEqual(stats['input_breakdown_estimated'], after)
        self.assertGreater(stats['usage']['prompt_tokens'], stats['initial_input_estimated_tokens'])
        self.assertNotIn('PRIVATE_', json.dumps(stats))
        self.assertEqual(bridge.input_breakdown(dict(body, tools=[]), 'native')['adapter_instructions'], 0)

    def test_prompt_mode_drops_client_system_instructions(self):
        body = dict(self.body, messages=[
            {'role': 'system', 'content': 'PRIVATE_SYSTEM'},
            {'role': 'user', 'content': 'do it'}])
        with patch.object(bridge, 'upstream', return_value=[
                {'type': 'text-delta', 'delta': json.dumps({'content': 'ok', 'tool_calls': []})}]) as request:
            bridge.completion(body)
        payload = request.call_args.args[0]
        self.assertNotIn('PRIVATE_SYSTEM', json.dumps(payload))
        self.assertIn('unattended', payload['systemPrompt'])
        self.assertEqual(bridge.input_breakdown(body, 'prompt')['client_instructions'], 0)

    def test_no_tools_uses_plain_text_without_an_envelope(self):
        body = {'model': 'gpt-6.1-sol', 'messages': [
            {'role': 'system', 'content': 'Be brief.'},
            {'role': 'user', 'content': 'Reply with ```json\n{"ok":true}\n```'}]}
        with patch.object(bridge, 'upstream', return_value=[{'type': 'text-delta', 'delta': '```json\n{"ok":true}\n```'}]) as request:
            result = bridge.completion(body)
        self.assertEqual(result['choices'][0]['message']['content'], '```json\n{"ok":true}\n```')
        payload = request.call_args.args[0]
        self.assertEqual(payload['systemPrompt'], 'Be brief.')
        self.assertEqual([m['role'] for m in payload['messages']], ['user'])
        self.assertNotIn('tools', payload)

    def test_no_tools_keeps_an_earlier_tool_result_as_text(self):
        body = {'messages': [{'role': 'user', 'content': 'hi'},
                             {'role': 'tool', 'tool_call_id': 'x', 'content': 'file body'}]}
        with patch.object(bridge, 'upstream', return_value=[{'type': 'text-delta', 'delta': 'ok'}]) as request:
            result = bridge.completion(body)
        self.assertEqual(result['choices'][0]['message']['content'], 'ok')
        self.assertEqual(request.call_args.args[0]['messages'][1]['parts'][0]['text'],
                         'Tool result (x):\nfile body')

    def test_tool_line_keeps_argument_names_types_and_required_marks(self):
        line = bridge.tool_line({'name': 'edit', 'description': 'Replace text in a file. Use exact matches.',
                                 'parameters': {'type': 'object', 'required': ['filePath'], 'properties': {
                                     'filePath': {'type': 'string'}, 'mode': {'type': 'string', 'enum': ['a', 'b']},
                                     'files': {'type': 'array', 'items': {'type': 'string'}}, 'extra': {}}}})
        self.assertEqual(line, 'edit(filePath*: string, mode: enum(a|b), files: string[], extra: any) - '
                               'Replace text in a file.')

    def test_failure_log_records_the_rejected_request_size(self):
        note = bridge.request_size_note({'messages': [{'role': 'user', 'content': 'x' * 300}],
                                         'tools': [{'type': 'function'}]})
        self.assertRegex(note, r' \(client request ~\d+ tokens, 1 messages, 1 tools\)$')
        self.assertEqual(bridge.request_size_note(None), ' (client request size unknown)')
        odd = bridge.request_size_note({'messages': None, 'tools': 3})
        self.assertRegex(odd, r' \(client request ~\d+ tokens, 0 messages, 0 tools\)$')

    def test_seventy_eight_tools_fit_well_inside_the_window(self):
        # Long OpenCode and MCP descriptions; the full schemas of this set were ~52k estimated tokens.
        tools = []
        for n in range(78):
            props = {f'param_{i}': {'type': 'string', 'description': 'A long explanation of this argument. ' * 6}
                     for i in range(5)}
            tools.append({'type': 'function', 'function': {'name': f'tool_{n}', 'description': (
                'Performs one operation on the connected service and returns its result. ' * 8),
                'parameters': {'type': 'object', 'properties': props, 'required': ['param_0']}}})
        body = dict(self.body, tools=tools)
        prompt = bridge.transport_instructions(body)
        self.assertEqual(sum(f'tool_{n}(' in prompt for n in range(78)), 78)
        self.assertLess(bridge.estimate_tokens(prompt), 8000)
        self.assertIn('tool_77(param_0*: string, param_1: string, param_2: string, param_3: string, param_4: string)', prompt)

    def test_prompt_protocol_matches_the_transport_prompt_file(self):
        from pathlib import Path
        prompt = (Path(__file__).parent / 'transport-prompt.txt').read_text().rstrip()
        self.assertTrue(bridge.transport_instructions(self.body).startswith(prompt + ' Available client tools: '))


    def tools(self):
        return [
            {'type': 'function', 'function': {'name': 'bash', 'description': 'Run a shell command.', 'parameters': {}}},
            {'type': 'function', 'function': {'name': 't3_thread_read', 'description': 'Read a thread.', 'parameters': {}}},
        ]

    def test_core_tools_only_when_the_client_offers_mcp_tools(self):
        prompt = bridge.transport_instructions({'tools': self.tools(), 'tool_choice': 'auto'})
        self.assertIn('bash(', prompt)
        self.assertNotIn('t3_thread_read(', prompt)

    def test_named_tool_choice_is_listed_even_when_not_core(self):
        prompt = bridge.transport_instructions({'tools': self.tools(),
                                                'tool_choice': {'type': 'function', 'function': {'name': 't3_thread_read'}}})
        self.assertIn('bash(', prompt)
        self.assertEqual(prompt.count('t3_thread_read('), 1)

    def test_required_tool_choice_lists_only_core(self):
        prompt = bridge.transport_instructions({'tools': self.tools(), 'tool_choice': 'required'})
        self.assertIn('bash(', prompt)
        self.assertNotIn('t3_thread_read(', prompt)

    def test_unlisted_client_tool_is_still_accepted(self):
        body = dict(self.body, tools=self.tools())
        events = [{'type': 'text-delta', 'delta': json.dumps(
            {'content': None, 'tool_calls': [{'name': 't3_thread_read', 'arguments': {}}]})}]
        with patch.object(bridge, 'upstream', return_value=events):
            result = bridge.completion(body)
        self.assertEqual(result['choices'][0]['message']['tool_calls'][0]['function']['name'], 't3_thread_read')

    def test_list_all_tools_disables_the_cap(self):
        with patch.object(bridge, 'LIST_ALL_TOOLS', True):
            prompt = bridge.transport_instructions({'tools': self.tools(), 'tool_choice': 'auto'})
        self.assertIn('t3_thread_read(', prompt)

    def test_input_breakdown_counts_only_listed_tools(self):
        body = dict(self.body, tools=self.tools())
        breakdown = bridge.input_breakdown(body, 'prompt')
        full = bridge.estimate_tokens(bridge.compact_json(body['tools']))
        self.assertLess(breakdown['tool_definitions'], full)


if __name__ == '__main__':
    unittest.main()
