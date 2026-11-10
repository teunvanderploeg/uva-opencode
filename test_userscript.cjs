const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const test = require('node:test');
const source = fs.readFileSync(__dirname + '/uva-opencode.user.js', 'utf8');
const tick = () => new Promise(resolve => setImmediate(resolve));

function harness({ origin = 'https://aichat.uva.nl', failFirst = false } = {}) {
  const store = new Map();
  const calls = [], upstream = [], timers = [], badges = [];
  let count = 0, cleaned = '';
  const token = 'test_relay_key_' + 'x'.repeat(30);
  const context = {
    URL, URLSearchParams, AbortController, Promise,
    location: { origin, pathname: '/chat', search: '', hash: '#uva-opencode=' + token },
    history: { state: {}, replaceState: (_state, _title, url) => { cleaned = url; } },
    document: { createElement: () => ({ style: {} }), documentElement: { appendChild: el => badges.push(el) } },
    GM_getValue: (key, fallback) => store.get(key) ?? fallback,
    GM_setValue: (key, value) => store.set(key, value),
    setTimeout: (fn, ms) => { timers.push({ fn, ms }); return timers.length; }, clearTimeout: () => {},
    fetch: async (url, options) => {
      upstream.push({ url: String(url), options });
      return { status: 200, text: async () => 'data: {"type":"text-delta","delta":"ok"}\n' +
        'data: {"type":"finish","finishReason":"stop","messageMetadata":{"systemPrompt":"private"}}\n' +
        'data: [DONE]\n' };
    },
    GM_xmlhttpRequest: options => {
      calls.push(options);
      if (options.method === 'POST') return queueMicrotask(() => options.onload({ status: 200, responseText: '{}' }));
      count++;
      if (failFirst && count === 1) return queueMicrotask(options.onerror);
      if (count > (failFirst ? 2 : 1)) return;
      queueMicrotask(() => options.onload({ status: 200, responseText: JSON.stringify({
        job: { id: 'test-job', endpoint: 'generic', payload: { messages: [] } },
      }) }));
    },
  };
  vm.runInNewContext(source, context);
  return { store, calls, upstream, timers, badges, token, cleaned: () => cleaned };
}

test('pairs locally and relays without exporting cookies or server metadata', async () => {
  const h = harness();
  for (let i = 0; i < 8; i++) await tick();
  assert.equal(h.store.get('relay-token'), h.token);
  assert.equal(h.cleaned(), '/chat');
  assert.equal(h.upstream[0].url, 'https://aichat.uva.nl/api/v1/chat/generic');
  assert.equal(h.upstream[0].options.credentials, 'same-origin');
  assert.equal(h.calls[0].anonymous, true);
  const result = h.calls.find(call => call.method === 'POST');
  assert.ok(result);
  assert.equal(result.url, 'http://127.0.0.1:8765/relay/result');
  assert.equal(result.headers.Authorization, 'Bearer ' + h.token);
  assert.ok(!result.data.includes('private'));
  assert.ok(!result.data.includes('systemPrompt'));
});

test('reconnects when the bridge becomes available', async () => {
  const h = harness({ failFirst: true });
  for (let i = 0; i < 3; i++) await tick();
  const retry = h.timers.find(timer => timer.ms === 3000);
  assert.ok(retry);
  retry.fn();
  for (let i = 0; i < 8; i++) await tick();
  assert.equal(h.upstream.length, 1);
  assert.ok(h.calls.some(call => call.method === 'POST'));
});

test('does nothing on any other origin', async () => {
  const h = harness({ origin: 'https://example.com' });
  await tick();
  assert.equal(h.calls.length, 0);
  assert.equal(h.store.size, 0);
  assert.equal(h.badges.length, 0);
});
