// Runs in the page's own world, so its fetch goes through the page's service worker (a content script's
// fetch does not). The page feed asks for a file the page makes itself and passes the pieces on. One request
// at a time per token; "next" asks for the next piece (about 512 KB), "stop" ends it.
(() => {
  const PIECE = 512 * 1024;
  const runs = new Map(); // token -> { reader, rest }: the body and bytes read past the last piece
  window.addEventListener('message', async e => {
    const m = e.data;
    if (e.source !== window || !m || typeof m.ndm !== 'string' || !m.token) return;
    const reply = (x, transfer) => window.postMessage({ ndm: 'got', token: m.token, ...x }, '*', transfer || []);
    if (m.ndm === 'fetch') {
      let r;
      try {
        r = await fetch(m.url, { credentials: 'include' });
      } catch (err) {
        return reply({ kind: 'error', why: String(err) });
      }
      if (r.body) runs.set(m.token, { reader: r.body.getReader(), rest: new Uint8Array(0) });
      return reply({ kind: 'head', ok: r.ok && !!r.body, status: r.status, type: r.headers.get('content-type') || '', len: Number(r.headers.get('content-length')) || 0 });
    }
    const run = runs.get(m.token);
    if (!run) return;
    if (m.ndm === 'stop') {
      runs.delete(m.token);
      return run.reader.cancel().catch(() => {});
    }
    if (m.ndm !== 'next') return;
    try {
      // pieces of exactly PIECE bytes (the last one shorter): the app takes requests up to 1 MB
      const parts = [run.rest];
      let have = run.rest.length;
      while (have < PIECE) {
        const { done, value } = await run.reader.read();
        if (done) break;
        parts.push(value);
        have += value.length;
      }
      if (!have) {
        runs.delete(m.token);
        return reply({ kind: 'end' });
      }
      const all = new Uint8Array(have);
      let o = 0;
      for (const p of parts) { all.set(p, o); o += p.length; }
      const bytes = all.slice(0, PIECE);
      run.rest = all.slice(bytes.length);
      reply({ kind: 'data', bytes }, [bytes.buffer]);
    } catch (err) {
      runs.delete(m.token);
      reply({ kind: 'error', why: String(err) });
    }
  });
})();
