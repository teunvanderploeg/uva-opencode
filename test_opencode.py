#!/usr/bin/env python3
"""Run the installed OpenCode in a disposable project, without global config."""
import json
import argparse
import os
import secrets
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent


def main(model='gpt-6-luna', scenario='copy'):
    executable = shutil.which('opencode')
    if not executable:
        raise SystemExit('OpenCode is not installed or is not on PATH.')
    with tempfile.TemporaryDirectory(prefix='uva-opencode-demo-') as folder:
        project = Path(folder).resolve()
        nonce = 'UVA_OPENCODE_' + secrets.token_hex(12)
        (project / 'challenge.txt').write_text(nonce + '\n')
        config = json.loads((ROOT / 'opencode.example.json').read_text())
        if model not in config['providers']['uva']['models']:
            raise SystemExit('Choose a model from opencode.example.json.')
        config.update(snapshots=False, warming=False, default_agent='uva-probe')
        config['agents'] = {'uva-probe': {
            'description': 'Read and write the disposable connection-test files only.',
            'mode': 'primary', 'steps': 6,
            'system': 'You are testing a provider adapter. Use client tools to read challenge.txt '
                      'and write the exact value to answer.txt. Do not invent file contents. '
                      'Do not use shell, network, or subagents. Give a short final answer.',
            'permissions': [
                {'action': '*', 'resource': '*', 'effect': 'deny'},
                {'action': 'read', 'resource': 'challenge.txt', 'effect': 'allow'},
                {'action': 'read', 'resource': 'answer.txt', 'effect': 'allow'},
                {'action': 'edit', 'resource': 'answer.txt', 'effect': 'allow'},
            ]}}
        prompt = ('Read challenge.txt with a tool. Write its exact contents '
                  'to answer.txt with a tool. Then say TEST_COMPLETE.')
        agent = 'uva-probe'
        if scenario == 'website':
            # Exercise OpenCode's normal Build instructions, rather than the tiny probe agent.
            agent = 'build'
            config['default_agent'] = agent
            config['agents'] = {'build': {'steps': 10, 'permissions': [
                {'action': '*', 'resource': '*', 'effect': 'deny'},
                {'action': 'read', 'resource': '*', 'effect': 'allow'},
                {'action': 'glob', 'resource': '*', 'effect': 'allow'},
                {'action': 'edit', 'resource': '*.html', 'effect': 'allow'},
                {'action': 'edit', 'resource': '*.css', 'effect': 'allow'},
                {'action': 'edit', 'resource': '*.js', 'effect': 'allow'},
            ]}}
            prompt = ('Create a simple good looking website for UvA with the logo and a fresh new look. '
                      'Make it about the Security and Network Engineering OS3 master. Use HTML and '
                      'whatever CSS style you think fits. Create index.html in this project using '
                      'the file tools. Keep it self-contained; use a text wordmark instead of fetching '
                      'a logo. Read challenge.txt and include its verification value in an HTML comment. '
                      'Do not just paste HTML in the chat. Do not use shell or subagents.')
        (project / 'opencode.json').write_text(json.dumps(config))
        environment = os.environ.copy()
        for key in list(environment):
            if key.startswith(('OPENCODE_', 'OPENAI_', 'ANTHROPIC_', 'GOOGLE_', 'GEMINI_')):
                environment.pop(key)
        for name, leaf in [('XDG_DATA_HOME', 'data'), ('XDG_CONFIG_HOME', 'config'),
                           ('XDG_CACHE_HOME', 'cache'), ('XDG_STATE_HOME', 'state')]:
            environment[name] = str(project / '.isolated' / leaf)
        environment['UVA_BRIDGE_KEY'] = (ROOT / '.runtime/local-token').read_text().strip()
        environment['PWD'] = str(project)
        command = [executable, 'run', '--standalone', '--format', 'json', '--model', 'uva/' + model,
                   '--agent', agent, prompt]
        completed = subprocess.run(command, cwd=project, env=environment,
                                   capture_output=True, text=True, timeout=240)
        # Test fixtures and OpenCode events contain no UvA credentials.
        events = completed.stdout
        suffix = '-website' if scenario == 'website' else ''
        (ROOT / f'.runtime/opencode{suffix}-events.jsonl').write_text(events)
        (ROOT / f'.runtime/opencode{suffix}-stderr.txt').write_text(completed.stderr)
        answer = project / ('index.html' if scenario == 'website' else 'answer.txt')
        decoded = []
        for line in events.splitlines():
            try:
                decoded.append(json.loads(line))
            except ValueError:
                continue
        tool_events = [e['part'] for e in decoded if e.get('type') == 'tool_use']
        completed_tools = [e['tool'] for e in tool_events if e.get('state', {}).get('status') == 'completed']
        actual = answer.read_text() if answer.exists() else ''
        matches = nonce in actual and '<html' in actual.lower() if scenario == 'website' else actual.strip() == nonce
        success = (completed.returncode == 0 and answer.exists() and matches
                   and 'read' in completed_tools and any(t in completed_tools for t in ('patch', 'write', 'edit')))
        report = {'opencode_exit_code': completed.returncode, 'file_contents_match': success,
                  'mode': 'prompt', 'native_tools': False, 'isolated_project': True,
                  'completed_tools': completed_tools, 'model': model, 'scenario': scenario,
                  'output_bytes': len(actual.encode())}
        (ROOT / f'.runtime/opencode{suffix}-result.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report, indent=2), flush=True)
        if not success:
            print('OpenCode output:', events[-12000:])
            print('OpenCode stderr:', completed.stderr[-3000:])
            raise SystemExit(1)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--model', default='gpt-6-luna')
    parser.add_argument('--scenario', choices=['copy', 'website'], default='copy')
    args = parser.parse_args()
    main(args.model, args.scenario)
