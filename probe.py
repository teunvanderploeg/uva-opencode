#!/usr/bin/env python3
"""Exercise chat and a client-tool round trip through the local adapter."""
import argparse
import json
import secrets
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parent


def call(path, payload=None):
    token = (ROOT / '.runtime/local-token').read_text().strip()
    request = Request('http://127.0.0.1:8765' + path,
                      data=None if payload is None else json.dumps(payload).encode(),
                      headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    try:
        with urlopen(request, timeout=170) as response:
            return json.load(response)
    except HTTPError as error:
        raise RuntimeError(error.read().decode()) from None


def run(mode, model='gpt-6-luna'):
    normal = call('/v1/chat/completions', {'model': model, 'uva_tool_mode': 'native',
                  'messages': [{'role': 'user', 'content': 'Reply with exactly UVA_BRIDGE_OK.'}]})
    assert normal['choices'][0]['message']['content'].strip() == 'UVA_BRIDGE_OK', normal
    print('Normal chat: PASS', flush=True)
    tools = [{'type': 'function', 'function': {
        'name': 'read_probe_file', 'description': 'Read the random verification value from a local test file.',
        'parameters': {'type': 'object', 'properties': {}, 'additionalProperties': False}}}]
    messages = [{'role': 'user', 'content':
                 'Call read_probe_file to get the verification value. After the result arrives, '
                 'reply with exactly that value. You cannot know the value before the tool executes.'}]
    initial = call('/v1/chat/completions', {'model': model, 'messages': messages,
                   'tools': tools, 'tool_choice': 'required', 'uva_tool_mode': mode})
    message = initial['choices'][0]['message']
    calls = message.get('tool_calls', [])
    assert len(calls) == 1 and calls[0]['function']['name'] == 'read_probe_file', initial
    assert json.loads(calls[0]['function']['arguments']) == {}, initial
    # Create the random value only after the model requests the tool.
    value = 'UVA_TOOL_' + secrets.token_hex(12)
    fixture = ROOT / '.runtime/probe-value.txt'
    fixture.write_text(value)
    actual = fixture.read_text()
    messages += [message, {'role': 'tool', 'tool_call_id': calls[0]['id'], 'content': actual}]
    final = call('/v1/chat/completions', {'model': model, 'messages': messages,
                 'tools': tools, 'uva_tool_mode': mode})
    assert final['choices'][0]['message']['content'].strip() == actual, final
    print(f'Tool request, execution, and result: PASS, mode={mode}', flush=True)
    return {'chat': 'pass', 'tool_cycle': 'pass', 'mode': mode}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--mode', choices=['native', 'prompt'], default='native')
    parser.add_argument('--model', default='gpt-6-luna')
    args = parser.parse_args()
    try:
        result = run(args.mode, args.model)
    except (RuntimeError, AssertionError) as error:
        print(f'FAILED: {error}', flush=True)
        raise SystemExit(1)
