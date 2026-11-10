// ==UserScript==
// @name         UvA OpenCode relay
// @namespace    local.teun.uva-opencode
// @version      0.2.1
// @description  Connect UvA Chat to your local OpenCode bridge. UvA cookies stay in Firefox.
// @match        https://aichat.uva.nl/*
// @noframes
// @run-at       document-idle
// @grant        GM_xmlhttpRequest
// @grant        GM_getValue
// @grant        GM_setValue
// @connect      127.0.0.1
// @updateURL    none
// ==/UserScript==

(() => {
  'use strict';
  if (location.origin !== 'https://aichat.uva.nl') return;
  const base = 'http://127.0.0.1:8765';
  const allowed = ['type', 'delta', 'toolCallId', 'toolName', 'input', 'finishReason'];
  const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
  const fragment = new URLSearchParams(location.hash.slice(1));
  const paired = fragment.get('uva-opencode');
  if (paired && /^[A-Za-z0-9_-]{32,128}$/.test(paired)) {
    GM_setValue('relay-token', paired);
    fragment.delete('uva-opencode');
    history.replaceState(history.state, '', location.pathname + location.search +
      (fragment.size ? '#' + fragment.toString() : ''));
  }

  const badge = document.createElement('div');
  badge.id = 'uva-opencode-status';
  Object.assign(badge.style, {
    position: 'fixed', right: '12px', bottom: '12px', zIndex: '2147483647',
    padding: '7px 11px', borderRadius: '6px', background: '#263746', color: '#fff',
    font: '13px system-ui', pointerEvents: 'none', boxShadow: '0 1px 5px #0003',
  });
  badge.textContent = 'OpenCode: waiting for launcher';
  document.documentElement.appendChild(badge);

  function local(path, token, body) {
    return new Promise((resolve, reject) => {
      GM_xmlhttpRequest({
        method: body === undefined ? 'GET' : 'POST', url: base + path,
        headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json' },
        anonymous: true, timeout: 26000,
        data: body === undefined ? undefined : JSON.stringify(body),
        onload(response) {
          if (response.status !== 200) return reject(new Error('Local bridge rejected the request.'));
          try { resolve(JSON.parse(response.responseText)); } catch { reject(new Error('Invalid local response.')); }
        },
        onerror: () => reject(new Error('Bridge unavailable.')),
        ontimeout: () => reject(new Error('Bridge timed out.')),
      });
    });
  }

  async function request(job) {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 120000);
    try {
      const path = job.endpoint === 'generic' ? '/api/v1/chat/generic' : '/api/v1/chat';
      const response = await fetch(new URL(path, location.origin), {
        method: 'POST', credentials: 'same-origin', redirect: 'error',
        headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(job.payload),
        signal: controller.signal,
      });
      const text = await response.text();
      const events = [];
      for (const line of text.split('\n')) {
        if (!line.startsWith('data:') || line.slice(5).trim() === '[DONE]') continue;
        const event = JSON.parse(line.slice(5));
        events.push(Object.fromEntries(allowed.filter(key => key in event).map(key => [key, event[key]])));
      }
      return { id: job.id, status: response.status, events };
    } catch {
      return { id: job.id, error: 'UvA browser request failed. Check login and network.' };
    } finally {
      clearTimeout(timer);
    }
  }

  async function run() {
    let connected = false;
    while (true) {
      const token = GM_getValue('relay-token', '');
      if (!token || !location.pathname.startsWith('/chat')) {
        badge.textContent = token ? 'OpenCode: sign in to UvA Chat' : 'OpenCode: waiting for launcher';
        await sleep(2000);
        continue;
      }
      try {
        badge.textContent = connected ? 'OpenCode: connected' : 'OpenCode: connecting';
        const { job } = await local('/relay/next', token);
        connected = true;
        badge.textContent = 'OpenCode: connected';
        if (!job) continue;
        badge.textContent = 'OpenCode: processing';
        const result = await request(job);
        await local('/relay/result', token, result);
      } catch {
        connected = false;
        badge.textContent = 'OpenCode: bridge stopped, waiting';
        await sleep(3000);
      }
    }
  }
  run();
})();
