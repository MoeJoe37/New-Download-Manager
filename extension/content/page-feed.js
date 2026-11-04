// A file the app can't get from the server as the page does (the page's service worker makes it, as on
// Telegram Web): page.js fetches it in the page's own world and this passes it to the background in pieces,
// which hands them to the app. "no" before anything is sent leaves the download to the browser.
(() => {
  if (window !== top) return;
  const b64 = bytes => {
    let s = '';
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(s);
  };
  chrome.runtime.onConnect.addListener(port => {
    if (port.name !== 'ndm-feed') return;
    const token = crypto.randomUUID();
    let wake = null; // page.js's next answer
    const ask = msg => new Promise(r => { wake = r; window.postMessage({ ...msg, token }, '*'); });
    const fromPage = e => {
      if (e.source === window && e.data && e.data.ndm === 'got' && e.data.token === token) wake?.(e.data);
    };
    window.addEventListener('message', fromPage);
    let gone = false, go = null;
    const stop = () => { window.removeEventListener('message', fromPage); window.postMessage({ ndm: 'stop', token }, '*'); };
    port.onDisconnect.addListener(() => { gone = true; stop(); go?.(); });
    port.onMessage.addListener(async m => {
      if (m.kind !== 'fetch') return go?.(); // "go" / "more" from the background
      // no answer from page.js (a browser without main-world scripts): the browser keeps the download
      const head = await Promise.race([ask({ ndm: 'fetch', url: m.url }), new Promise(r => setTimeout(() => r(null), 5000))]);
      const type = ((head && head.type) || '').split(';')[0].trim().toLowerCase();
      // the same file the browser started downloading: not an error, not a page, the same size
      if (!head || head.kind !== 'head' || !head.ok || /^(text\/html|application\/xhtml\+xml)$/.test(type) || (m.size > 0 && head.len > 0 && head.len !== m.size)) {
        stop();
        return port.postMessage({ kind: 'no', why: head ? `${head.status} ${type} ${head.len}` : 'no answer from the page' });
      }
      port.postMessage({ kind: 'yes', size: head.len || m.size || 0 });
      await new Promise(r => (go = r)); // "go": the app has the download in its list
      let at = 0;
      for (;;) {
        if (gone) return;
        const piece = await ask({ ndm: 'next' });
        if (piece.kind === 'end') return port.postMessage({ kind: 'end' });
        if (piece.kind !== 'data') return port.postMessage({ kind: 'fail', why: piece.why || piece.kind });
        port.postMessage({ kind: 'data', at, data: b64(piece.bytes) });
        at += piece.bytes.length;
        await new Promise(r => (go = r)); // "more": the app wrote it
      }
    });
  });
})();
