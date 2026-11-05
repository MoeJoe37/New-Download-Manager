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
  const initiallyHidden = hidden(await store.get(['panel_off', 'panel_sites_off']).catch(() => ({})));

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
  let target = null, hideTimer = 0, off = initiallyHidden;
  let media = { items: [], types: [], minimums: {} }, mediaRevision = 0;
  let url = location.href;
  const dismissed = new WeakMap();
  const encrypted = new WeakSet();
  const sources = new WeakMap();
  const source = el => el?.currentSrc || el?.src || el?.querySelector?.('source[src]')?.src || '';
  const playerKey = el => `${url}\n${source(el)}`;
  const isProtected = el => !!el && (el.mediaKeys || encrypted.has(el));
  const isWeb = value => /^https?:\/\//i.test(value || '');
  const eligible = el => el?.isConnected && (!isProtected(el) ? isWeb(source(el)) ||
    isVideoUrl(videoPage(el)) || media.items.some(item => item.stream) : panel_drm);
  const resize = typeof ResizeObserver === 'function' ? new ResizeObserver(() => schedulePlace()) : null;
  let placement = 0;
  const schedulePlace = () => {
    if (!placement) placement = requestAnimationFrame(() => { placement = 0; place(); });
  };

  const human = n => !n ? '' : n < 1048576 ? `${(n / 1024).toFixed(0)} KB` : `${(n / 1048576).toFixed(1)} MB`;
  const title = () => (document.title || 'video').replace(/[\\/:*?"<>|]+/g, '_').trim().slice(0, 120);


  function hide() {
    host.style.display = 'none';
    menu.style.display = 'none';
  }
  function place() {
    if (!target || off || !eligible(target) || dismissed.get(target) === playerKey(target)) return hide();
    const r = target.getBoundingClientRect();
    if (r.width < 120 || r.height < 40) return hide();
    host.style.display = 'block';
    // inside a full-screen element the panel sits on the screen itself, else on the page
    const fixed = host.parentNode !== document.documentElement;
    host.style.position = fixed ? 'fixed' : 'absolute';
    host.style.top = `${(fixed ? 0 : window.scrollY) + r.top + 6}px`;
    host.style.left = `${(fixed ? 0 : window.scrollX) + r.right - host.offsetWidth - 6}px`;
  }
  async function refreshMedia() {
    const revision = ++mediaRevision;
    const result = await chrome.runtime.sendMessage({ kind: 'media-list' }).catch(() => null);
    if (revision === mediaRevision && Array.isArray(result?.items)) {
      media = result;
      scheduleScan();
    }
  }

  function choices(el) {
    const src = source(el);
    const direct = isWeb(src);
    const normalized = value => { const parsed = new URL(value, location.href); parsed.hash = ''; return parsed.href; };
    // For a direct player only attribute its own response. A MediaSource/blob player can
    // use a manifest captured in its frame. Never offer another player's direct file.
    const items = media.items.filter(item => direct ? normalized(item.url) === normalized(src) : item.stream);
    if (direct && !items.length) {
      const path = new URL(src, location.href).pathname;
      const ext = (path.match(/\.([a-z0-9]{2,5})$/i)?.[1] || '').toUpperCase();
      const stream = ext === 'M3U8' ? 'hls' : ext === 'MPD' ? 'dash' : '';
      if (stream || media.types.includes(ext)) items.push({ url: normalized(src), ext, stream, size: 0 });
    }
    return items.map(item => ({
      ...item,
      resolution: direct && el.videoWidth && el.videoHeight ? `${el.videoWidth}×${el.videoHeight}` : '',
    }));
  }

  async function download(m) {
    menu.style.display = 'none';
    const filename = m.output ? `${title()}.${m.output}` : m.stream ? '' : m.ext ? `${title()}.${m.ext.toLowerCase()}` : '';
    const ok = await chrome.runtime.sendMessage({ kind: 'download', url: m.url, filename,
      size: m.size || 0, mime: m.mime || '', stream: m.stream || '', output: m.output, variant: m.variant }).catch(() => false);
    if (!ok && (m.stream || m.output)) say('Could not send this format to New Download Manager');
  }
  function show(el) {
    if (off || !eligible(el) || dismissed.get(el) === playerKey(el)) return;
    if (target !== el) {
      resize?.disconnect();
      resize?.observe(el);
      menu.style.display = 'none';
    }
    target = el;
    label.textContent = el.tagName === 'AUDIO' ? 'Download this audio' : 'Download this video';
    const holder = document.fullscreenElement || document.documentElement;
    if (host.parentNode !== holder) holder.appendChild(host);
    clearTimeout(hideTimer);
    place();
  }

  bar.addEventListener('click', async e => {
    e.stopPropagation();
    // the extension was updated or reloaded since this page opened: this copy can't reach it any more
    if (!chrome.runtime?.id) return say('Reload the page to use New Download Manager');
    if (e.target.classList.contains('x')) {
      if (target) dismissed.set(target, playerKey(target));
      return hide();
    }
    const player = target;
    if (!player || isProtected(player)) return say('This media is protected');
    const clicked = playerKey(player);
    // YouTube and other sites that play videos in their own player: the stream pieces the page fetches
    // aren't the video, so the app gets the page and its yt-dlp finds the video
    const page = await chrome.runtime.sendMessage({ kind: 'video-page', url: videoPage(player) }).catch(() => false);
    if (page) return say(page === 'sent' ? 'Sent to New Download Manager' : 'Start New Download Manager first');
    await refreshMedia();
    if (target !== player || clicked !== playerKey(player)) return;
    const found = choices(player);
    const manifests = found.filter(item => item.stream || item.mime === 'video/mp2t').slice(0, 3);
    if (manifests.length) say('Checking stream formats…');
    const variants = await Promise.all(manifests.map(async item => {
      const result = await chrome.runtime.sendMessage({ kind: 'formats', url: item.url, stream: item.stream, mime: item.mime }).catch(() => []);
      return (Array.isArray(result) ? result : []).flatMap(format => {
        const outputs = (format.outputs || []).filter(ext => media.types.includes(ext.toUpperCase()));
        return outputs.map(output => ({ ...item, ...format, output }));
      });
    }));
    if (target !== player || clicked !== playerKey(player)) return;
    const items = [...found, ...variants.flat()];
    if (!items.length) {
      // A known video page can use its extractor. An unknown player with no usable source
      // should not save a random response from the frame.
      if (!isVideoUrl(videoPage(player))) return say('No downloadable format detected');
      const any = await chrome.runtime.sendMessage({ kind: 'video-page', url: videoPage(player), any: true }).catch(() => false);
      return say(any === 'sent' ? 'Sent to New Download Manager' : 'Start New Download Manager first');
    }
    if (items.length === 1) return download(items[0]);
    menu.replaceChildren(...items.map(m => {
      const d = document.createElement('div');
      const format = m.output
        ? `${m.output.toUpperCase()} (${m.output === 'mp4' && m.transcode ? 'convert' : 'assemble/remux'})`
        : m.stream ? `${m.stream.toUpperCase()} stream (automatic container)` : m.ext;
      d.textContent = [format, m.resolution, m.bandwidth ? `${Math.round(m.bandwidth / 1000)} kbps` : '', m.codecs, m.audio ? 'audio' : '', human(m.size), new URL(m.url).pathname.split('/').pop()].filter(Boolean).join('  ');
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
      item('Hide for now', () => { if (target) dismissed.set(target, playerKey(target)); hide(); }),
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
      resize?.disconnect();
      hide();
    } else {
      scheduleScan();
    }
  });
  host.addEventListener('mouseenter', () => clearTimeout(hideTimer));
  host.addEventListener('mouseleave', () => (hideTimer = setTimeout(hide, 1500)));

  // Observe player lifecycle and source changes. Coalesce mutations and layout work
  // to animation frames. Scrolling only repositions the active panel.
  let scanPending = false;
  function scheduleScan() {
    if (scanPending) return;
    scanPending = true;
    requestAnimationFrame(() => {
      scanPending = false;
      if (url !== location.href) {
        url = location.href;
        media = { ...media, items: [] };
        refreshMedia();
        menu.style.display = 'none';
      }
      if (target && eligible(target) && dismissed.get(target) !== playerKey(target)) {
        if (sources.get(target) !== source(target)) {
          sources.set(target, source(target));
          menu.style.display = 'none';
          show(target);
        } else if (!target.paused) {
          show(target);
        }
        return;
      }
      target = null;
      resize?.disconnect();
      hide();
      const candidates = [...document.querySelectorAll('video,audio')].filter(el => eligible(el) && dismissed.get(el) !== playerKey(el));
      const player = candidates.find(el => !el.paused) || candidates.find(el => el.readyState >= 1);
      if (player) {
        sources.set(player, source(player));
        show(player);
      }
    });
  }

  const pointer = e => {
    const el = e.composedPath().find(node => node.matches?.('video,audio'));
    if (el && (el !== target || host.style.display === 'none')) show(el);
  };
  document.addEventListener('pointerover', pointer, true);
  for (const event of ['play', 'playing', 'loadedmetadata', 'loadstart', 'emptied']) {
    document.addEventListener(event, e => {
      if (!e.target.matches?.('video,audio')) return;
      if (event === 'play' || event === 'playing') show(e.target);
      scheduleScan();
    }, true);
  }
  document.addEventListener('encrypted', e => {
    encrypted.add(e.target);
    scheduleScan();
  }, true);
  const containsPlayer = node => node.matches?.('video,audio,source') || node.querySelector?.('video,audio,source');
  new MutationObserver(records => {
    if (records.some(record => record.type === 'attributes' ? containsPlayer(record.target) :
      [...record.addedNodes, ...record.removedNodes].some(containsPlayer))) scheduleScan();
  }).observe(document.documentElement, { subtree: true, childList: true, attributes: true, attributeFilter: ['src'] });
  window.addEventListener('scroll', schedulePlace, { capture: true, passive: true });
  document.addEventListener('fullscreenchange', () => target && show(target));
  window.addEventListener('resize', schedulePlace);
  window.addEventListener('popstate', scheduleScan);
  window.addEventListener('hashchange', scheduleScan);
  chrome.runtime.onMessage.addListener(m => {
    if (m.kind === 'media') refreshMedia();
    if (m.kind === 'ndm-navigation') {
      mediaRevision++;
      media = { ...media, items: [] };
      menu.style.display = 'none';
      scheduleScan();
      refreshMedia();
    }
  });
  refreshMedia();
  scheduleScan();
})();
