async ({endpoint, payload}) => {
  const controller = new AbortController();
  globalThis.__uvaRequest = controller;
  const timer = setTimeout(() => controller.abort(), 120000);
  try {
    const path = endpoint === 'generic' ? '/api/v1/chat/generic' : '/api/v1/chat';
    const response = await fetch(path, {
      method: 'POST', credentials: 'same-origin', redirect: 'error',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify(payload), signal: controller.signal
    });
    if (response.status !== 200) return {status: response.status, stream: ''};
    const reader = response.body.getReader();
    const decoder = new TextDecoder();
    let stream = '';
    for (;;) {
      const {done, value} = await reader.read();
      if (done) break;
      stream += decoder.decode(value, {stream: true});
      if (stream.length > 8000000) {
        await reader.cancel();
        throw new Error('Upstream response too large');
      }
    }
    stream += decoder.decode();
    return {status: response.status, stream};
  } finally {
    clearTimeout(timer);
    if (globalThis.__uvaRequest === controller) delete globalThis.__uvaRequest;
  }
}
