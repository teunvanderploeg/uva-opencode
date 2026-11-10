#!/usr/bin/env python3
"""Small live checks for models copied from the signed-in UvA model selector."""
import json
from datetime import datetime, timezone

from bridge import ROOT, model_catalog
from probe import call


def main():
    results = []
    for model in model_catalog():
        entry = {'model': model}
        try:
            response = call('/v1/chat/completions', {
                'model': model, 'uva_tool_mode': 'prompt',
                'messages': [{'role': 'user', 'content': 'Reply with exactly UVA_MODEL_OK.'}],
            })
            text = response['choices'][0]['message']['content']
            if text.strip().rstrip('.') != 'UVA_MODEL_OK':
                raise ValueError('Response did not match the requested marker.')
            entry['chat_and_json_transport'] = 'passed'
        except (RuntimeError, ValueError, KeyError, TypeError) as error:
            entry['chat_and_json_transport'] = 'failed'
            entry['error'] = str(error)
        results.append(entry)
        print(json.dumps(entry), flush=True)
        report = {'checked_at': datetime.now(timezone.utc).isoformat(),
                  'source': 'Signed-in UvA AI Chat model selector, including legacy models',
                  'mode': 'prompt', 'results': results}
        (ROOT / 'model-validation.json').write_text(json.dumps(report, indent=2) + '\n')
    return int(any(entry['chat_and_json_transport'] != 'passed' for entry in results))


if __name__ == '__main__':
    raise SystemExit(main())
