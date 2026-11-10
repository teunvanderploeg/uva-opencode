// Run only in a signed-in https://aichat.uva.nl tab. No UvA cookies are read or exported.
(() => {
  if (location.origin !== 'https://aichat.uva.nl') throw new Error('Open the UvA Chat tab first.');
  if (globalThis.uvaRelayStop) globalThis.uvaRelayStop();
  const controller = new AbortController();
  globalThis.uvaRelayStop = () => controller.abort();
  const local = 'http://127.0.0.1:8765';
  const headers = { Authorization: 'Bearer __LOCAL_TOKEN__', 'Content-Type': 'application/json' };
  const allowedFields = ['type', 'delta', 'toolCallId', 'toolName', 'input', 'finishReason'];
  async function run() {
    while (!controller.signal.aborted) {
      try {
        const next = await fetch(local + '/relay/next', { headers, signal: controller.signal });
        if (!next.ok) throw new Error('Local relay rejected the connection.');
        const { job } = await next.json();
        if (!job) continue;
        let result;
        try {
          const path = job.endpoint === 'generic' ? '/api/v1/chat/generic' : '/api/v1/chat';
          const upstream = await fetch(path, { method: 'POST', redirect: 'error',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(job.payload), signal: controller.signal });
          const text = await upstream.text();
          const events = [];
          for (const line of text.split('\n')) {
            if (!line.startsWith('data:') || line.slice(5).trim() === '[DONE]') continue;
            const event = JSON.parse(line.slice(5));
            events.push(Object.fromEntries(allowedFields.filter(k => k in event).map(k => [k, event[k]])));
          }
          result = { id: job.id, status: upstream.status, events };
        } catch {
          result = { id: job.id, error: 'UvA browser request failed. Check login and network.' };
        }
        await fetch(local + '/relay/result', { method: 'POST', headers,
          body: JSON.stringify(result), signal: controller.signal });
      } catch (error) {
        if (!controller.signal.aborted) console.error('UvA relay stopped:', error.message);
        break;
      }
    }
  }
  run();
  console.info('UvA relay started. Stop with uvaRelayStop() or reload this tab.');
})();
