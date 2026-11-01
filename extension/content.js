// Special keys: report Alt/Shift/Ctrl/Ins/Del to the background, and with
// "check for left mouse button" take a link clicked with the force keys held straight to the app.
(() => {
  const KEY = { Alt: 'alt', Shift: 'shift', Control: 'ctrl', Insert: 'ins', Delete: 'del' };
  const held = new Set();
  let keys = null;
  chrome.runtime.sendMessage({ kind: 'config' }).then(c => (keys = c && c.keys)).catch(() => {});
  const report = () => chrome.runtime.sendMessage({ kind: 'keys', held: [...held] }).catch(() => {});
  addEventListener('keydown', e => { const k = KEY[e.key]; if (k && !held.has(k)) { held.add(k); report(); } }, true);
  addEventListener('keyup', e => { const k = KEY[e.key]; if (k && held.delete(k)) report(); }, true);
  addEventListener('blur', () => { if (held.size) { held.clear(); report(); } });
  addEventListener('click', e => {
    if (!keys || !keys.force || !keys.check_mouse || e.button !== 0) return;
    const want = [['alt', keys.alt_f], ['shift', keys.shift_f], ['ctrl', keys.ctrl_f], ['ins', keys.ins_f]].filter(x => x[1]).map(x => x[0]);
    const a = e.target.closest && e.target.closest('a[href]');
    if (!a || !/^https?:/.test(a.href) || !want.length || !want.every(k => held.has(k))) return;
    if (keys.skip_html && /\.(html?|php|aspx?|jsp|jpe?g|png|gif|webp|svg|js|css)(\?|#|$)/i.test(new URL(a.href).pathname)) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    chrome.runtime.sendMessage({ kind: 'download', url: a.href });
  }, true);
})();

// "Download all links": every link on the page with its text, for the app's link list.
chrome.runtime.onMessage.addListener((m, _, reply) => {
  if (m.kind === 'links' && window === window.top) {
    const seen = new Set();
    reply([...document.links].filter(a => /^https?:/.test(a.href) && !seen.has(a.href) && seen.add(a.href))
      .map(a => ({ url: a.href, text: (a.textContent || '').trim().slice(0, 200) })));
  }
});

// Video grabber panel: a "Download this video" button over <video>/<audio> elements.
(async () => {
  if (window.__ndmPanel) return;
  window.__ndmPanel = true;
  // "Don't show download panel for the following sites" (space separated, * wildcards)
  const { panel_skip = '', panel_mini = false, panel_drm = true } = (await chrome.runtime.sendMessage({ kind: 'config' }).catch(() => null)) || {};
  const wild = p => new RegExp('^' + p.replace(/[.+?^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*') + '$', 'i');
  const site = location.hostname;
  if (panel_skip.split(/\s+/).filter(Boolean).some(p => wild(p).test(site) || site.endsWith('.' + p))) return;
  // the extension's own options (and the panel's right-click menu): off everywhere, or on these sites
  const store = chrome.storage.local;
  const hidden = s => s.panel_off || (s.panel_sites_off || []).includes(site);
  if (hidden(await store.get(['panel_off', 'panel_sites_off']).catch(() => ({})))) return;

  const host = document.createElement('div');
  host.style.cssText = 'position:absolute;z-index:2147483647;display:none;top:0;left:0';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = `<style>
    .bar{display:flex;width:max-content;align-items:center;gap:6px;font:12px Tahoma,Segoe UI,sans-serif;color:#fff;cursor:pointer;
      background:linear-gradient(#4a90e2,#1f5fb8);border:1px solid #174a8f;border-radius:3px;padding:4px 8px 4px 6px;
      box-shadow:0 1px 4px rgba(0,0,0,.45);user-select:none;white-space:nowrap}
    .bar:hover{background:linear-gradient(#5ea0ee,#2a6ccb)}
    .arrow{width:14px;height:14px;border-radius:50%;background:#fff;color:#1f5fb8;font-weight:bold;
      display:flex;align-items:center;justify-content:center;font-size:11px}
    .x{margin-left:4px;opacity:.8;padding:0 2px}.x:hover{opacity:1}
    .menu{display:none;width:max-content;margin-top:2px;background:#fff;color:#000;border:1px solid #999;font:12px Tahoma,sans-serif;
      box-shadow:2px 2px 4px rgba(0,0,0,.3);max-width:420px}
    .menu div{padding:4px 10px;cursor:pointer;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
    .menu div:hover{background:#cce8ff}
    .menu .hint{color:#666;cursor:default;font-size:11px;white-space:normal;max-width:230px}.menu .hint:hover{background:none}
    .menu hr{border:0;border-top:1px solid #ddd;margin:2px 0}</style>
    <div class="bar"><span class="arrow">&#8595;</span><span class="label">Download this video</span><span class="x" title="Hide">&#10005;</span></div>
    <div class="menu"></div>`;
  const bar = root.querySelector('.bar'), menu = root.querySelector('.menu'), label = root.querySelector('.label');
  const say = text => {
    label.textContent = text;
    clearTimeout(say.t);
    say.t = setTimeout(() => (label.textContent = target && target.tagName === 'AUDIO' ? 'Download this audio' : 'Download this video'), 3000);
  };
  if (panel_mini) label.style.display = 'none'; // Panel view: Mini mode
  let target = null, hideTimer = 0, off = false;
  const dismissed = new WeakSet();

  const human = n => !n ? '' : n < 1048576 ? `${(n / 1024).toFixed(0)} KB` : `${(n / 1048576).toFixed(1)} MB`;
  const title = () => (document.title || 'video').replace(/[\\/:*?"<>|]+/g, '_').trim().slice(0, 120);
  const ext = m => /^(hls|dash)$/.test(m.type) ? 'mp4' : (m.url.split('?')[0].match(/\.(\w{2,4})$/) || [0, (m.type || '').split('/').pop() || 'mp4'])[1];

  function hide() {
    host.style.display = 'none';
    menu.style.display = 'none';
  }
  function place() {
    if (!target || !target.isConnected) return hide();
    const r = target.getBoundingClientRect();
    if (r.width < 120 || r.height < 40) return hide();
    host.style.display = 'block';
    host.style.top = `${window.scrollY + r.top + 6}px`;
    host.style.left = `${window.scrollX + r.right - host.offsetWidth - 6}px`;
  }
  async function list() {
    const items = (await chrome.runtime.sendMessage({ kind: 'list' }).catch(() => [])) || [];
    const src = target && target.currentSrc;
    if (src && /^https?:/.test(src) && !items.some(m => m.url === src)) items.unshift({ url: src, type: target.tagName === 'AUDIO' ? 'audio' : 'video', size: 0 });
    return items;
  }
  function download(m) {
    // the size and type the page got, so the app can tell whether it gets the same file
    chrome.runtime.sendMessage({ kind: 'download', url: m.url, filename: `${title()}.${ext(m)}`, size: m.size || 0, mime: /\//.test(m.type) ? m.type : '' });
    menu.style.display = 'none';
  }
  function show(el) {
    if (off || dismissed.has(el)) return;
    if (el.mediaKeys && !panel_drm) return; // protected (EME) content: "Show download panel for protected content" off
    target = el;
    label.textContent = el.tagName === 'AUDIO' ? 'Download this audio' : 'Download this video';
    if (!host.isConnected) document.documentElement.appendChild(host);
    clearTimeout(hideTimer);
    place();
  }

  bar.addEventListener('click', async e => {
    e.stopPropagation();
    if (e.target.classList.contains('x')) {
      dismissed.add(target);
      return hide();
    }
    // YouTube and other sites that play videos in their own player: the stream pieces the page fetches
    // aren't the video, so the app gets the page and its yt-dlp finds the video
    const page = await chrome.runtime.sendMessage({ kind: 'video-page', url: location.href }).catch(() => false);
    if (page) return say(page === 'sent' ? 'Sent to New Download Manager' : 'Start New Download Manager first');
    const items = await list();
    if (!items.length) {
      // nothing played yet, or a player that streams in pieces: the page goes to yt-dlp, which knows
      // most players without the video having to play
      const any = await chrome.runtime.sendMessage({ kind: 'video-page', url: location.href, any: true }).catch(() => false);
      return say(any === 'sent' ? 'Sent to New Download Manager' : 'Start New Download Manager first');
    }
    if (items.length === 1) return download(items[0]);
    menu.replaceChildren(...items.map(m => {
      const d = document.createElement('div');
      d.textContent = `${(m.type || 'media').toUpperCase()}  ${human(m.size)}  ${m.url.split('?')[0].split('/').pop()}`;
      d.title = m.url;
      d.onclick = ev => { ev.stopPropagation(); download(m); };
      return d;
    }));
    menu.style.display = 'block';
  });
  // right click: hide it now, on this site, or everywhere (the extension's options bring it back)
  bar.addEventListener('contextmenu', e => {
    e.preventDefault();
    e.stopPropagation();
    const item = (text, act) => {
      const d = document.createElement('div');
      d.textContent = text;
      d.onclick = async ev => {
        ev.stopPropagation();
        menu.style.display = 'none';
        await act();
      };
      return d;
    };
    const sites = async () => (await store.get('panel_sites_off').catch(() => ({}))).panel_sites_off || [];
    const hint = document.createElement('div');
    hint.className = 'hint';
    hint.textContent = "Turn it back on in the extension's options (right-click the extension's button).";
    menu.replaceChildren(
      item('Hide for now', () => { dismissed.add(target); hide(); }),
      item(`Don't show on ${site}`, async () => { await store.set({ panel_sites_off: [...new Set([...(await sites()), site])] }); hide(); }),
      item('Disable the download panel', async () => { await store.set({ panel_off: true }); hide(); }),
      document.createElement('hr'),
      item('Extension options...', () => chrome.runtime.sendMessage({ kind: 'options' }).catch(() => {})),
      hint,
    );
    menu.style.display = 'block';
  });
  // turned off from the options or another tab: gone at once
  chrome.storage.onChanged.addListener(async () => {
    off = hidden(await store.get(['panel_off', 'panel_sites_off']).catch(() => ({})));
    if (off) {
      target = null;
      hide();
    }
  });
  host.addEventListener('mouseenter', () => clearTimeout(hideTimer));
  host.addEventListener('mouseleave', () => (hideTimer = setTimeout(hide, 1500)));

  // Show over a media element when the pointer is on it, or when it starts playing.
  document.addEventListener('mouseover', e => {
    const el = [...document.querySelectorAll('video,audio')].find(v => {
      const r = v.getBoundingClientRect();
      return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
    });
    if (el) show(el);
    else if (target && !host.matches(':hover')) {
      clearTimeout(hideTimer);
      hideTimer = setTimeout(hide, 1500);
    }
  }, true);
  document.addEventListener('play', e => e.target.matches && e.target.matches('video,audio') && show(e.target), true);
  window.addEventListener('scroll', place, true);
  window.addEventListener('resize', place);
  chrome.runtime.onMessage.addListener(m => {
    if (m.kind === 'media' && !target) {
      const v = document.querySelector('video');
      if (v) show(v);
    }
  });
})();

// Download panel for selected links: selecting text that holds links shows a button
// that hands those links to the app's link list.
(async () => {
  if (window !== window.top || window.__ndmSel) return;
  window.__ndmSel = true;
  const { sel_mode = 0, sel_sites = [], sel_mini = false } = (await chrome.runtime.sendMessage({ kind: 'config' }).catch(() => null)) || {};
  const site = location.hostname.toLowerCase();
  if (sel_mode === 1 || (sel_mode === 2 && !sel_sites.some(s => site === s || site.endsWith('.' + s)))) return;
  const host = document.createElement('div');
  host.style.cssText = 'position:absolute;z-index:2147483647;display:none';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = `<style>.b{font:12px Tahoma,Segoe UI,sans-serif;color:#fff;cursor:pointer;white-space:nowrap;user-select:none;
    background:linear-gradient(#4a90e2,#1f5fb8);border:1px solid #174a8f;border-radius:3px;padding:3px 8px;box-shadow:0 1px 4px rgba(0,0,0,.45)}
    .b:hover{background:linear-gradient(#5ea0ee,#2a6ccb)}</style><div class="b"></div>`;
  const b = root.querySelector('.b');
  let links = [];
  const hide = () => (host.style.display = 'none');
  b.addEventListener('mousedown', e => e.preventDefault()); // keep the selection
  b.addEventListener('click', () => {
    chrome.runtime.sendMessage({ kind: 'sel', links });
    hide();
  });
  document.addEventListener('mouseup', e => {
    if (e.composedPath().includes(host)) return;
    setTimeout(() => {
      const s = getSelection();
      const seen = new Set();
      links = s && !s.isCollapsed ? [...document.links].filter(a => /^https?:/.test(a.href) && s.containsNode(a, true) && !seen.has(a.href) && seen.add(a.href))
        .map(a => ({ url: a.href, text: (a.textContent || '').trim().slice(0, 200) })) : [];
      if (!links.length) return hide();
      b.textContent = sel_mini ? '↓' : `↓ Download selected links (${links.length})`;
      if (!host.isConnected) document.documentElement.appendChild(host);
      host.style.display = 'block';
      host.style.left = `${scrollX + e.clientX + 8}px`;
      host.style.top = `${scrollY + e.clientY + 12}px`;
    }, 0);
  }, true);
  document.addEventListener('mousedown', e => !e.composedPath().includes(host) && hide(), true);
})();

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
