// Downloads go to the app on 127.0.0.1; if it isn't running (or declines), the browser keeps them.
// The app listens on the first free one of these ports (Windows can reserve one for Hyper-V, WSL or Docker)
// and marks its replies "X-NDM: 1"; versions before 2.0 listen on 9614 only, without the mark.
const PORTS = [9614, 19614, 29614, 39614];
let port = PORTS[0];
// A request to the app: the port that answered last first, then the others. Throws when none answers.
async function ndm(path, init) {
  let old = null; // an unmarked reply on 9614: an older app, unless a marked one answers elsewhere
  for (const p of [port, ...PORTS.filter(x => x !== port)]) {
    try {
      const r = await fetch(`http://127.0.0.1:${p}${path}`, init);
      if (r.headers.get('X-NDM')) {
        port = p;
        return r;
      }
      if (p === PORTS[0]) old = r;
    } catch {}
  }
  if (old) return old;
  throw new Error('New Download Manager is not running');
}
// The file type of a media response, as named in the app's download panel list.
const MIME_TYPE = { 'audio/mpeg': 'MP3', 'video/mpeg': 'MPG', 'video/x-flv': 'FLV', 'video/quicktime': 'MOV', 'video/x-matroska': 'MKV',
  'video/x-ms-wmv': 'WMV', 'audio/x-ms-wma': 'WMA', 'video/x-ms-asf': 'ASF', 'video/3gpp': '3GP', 'audio/mp4': 'M4A', 'audio/x-m4a': 'M4A',
  'video/x-msvideo': 'AVI', 'audio/wav': 'WAV', 'audio/x-wav': 'WAV', 'audio/vnd.wave': 'WAV' };
function mediaType(mime, path, types) {
  const ext = ((path.match(/\.(\w{2,5})$/) || [])[1] || '').toUpperCase();
  const byMime = MIME_TYPE[mime] || (/^(video|audio)\//.test(mime) ? mime.split('/')[1].toUpperCase() : '');
  return [ext, byMime].find(t => t && types.includes(t)) || '';
}

// The app's native messaging host (ndm.exe itself, registered by the app at startup) starts the app when
// it isn't running.
const launch = () => chrome.runtime.sendNativeMessage('com.ndm.host', { kind: 'launch' }).catch(() => {});
chrome.action.onClicked.addListener(launch);

// The browser's own proxy, for the app's "Use proxy/socks from a browser's request" (Options -> Proxy / Socks).
async function browserProxy() {
  const v = (await chrome.proxy?.settings.get({}).catch(() => null))?.value || {};
  if (v.mode === 'pac_script') return v.pacScript?.url ? `pac:${v.pacScript.url}` : '';
  const r = v.rules?.singleProxy || v.rules?.proxyForHttps || v.rules?.proxyForHttp;
  if (v.mode === 'fixed_servers' && r) return `${/^socks[45]$/.test(r.scheme) ? r.scheme : 'http'}://${r.host}:${r.port || 80}`;
  // Firefox: { proxyType: 'manual', http: 'h:p', socks: 'h:p', socksVersion } or a PAC address
  if (v.proxyType === 'manual') return v.http ? `http://${v.http}` : v.socks ? `socks${v.socksVersion || 5}://${v.socks}` : '';
  return v.proxyType === 'autoConfig' && v.autoConfigUrl ? `pac:${v.autoConfigUrl}` : '';
}

// true: the app took it, false: the app declined it, 'page': the server sends the app another file than the
// browser gets (see `feed`), null: the app couldn't be reached (even after starting it).
// With extra.check the app first makes sure the server sends it the same file the browser gets (size, type);
// pages that make their files themselves (a service worker, as Telegram Web does), expired one-time links and
// login pages give the app something else, and those stay with the browser.
async function post(url, extra) {
  const cookies = (await chrome.cookies.getAll({ url }).catch(() => [])).map(c => `${c.name}=${c.value}`).join('; ');
  const body = JSON.stringify({ url, cookies, proxy: await browserProxy(), ...extra });
  for (let i = 0; i < 40; i++) {
    try {
      const r = await ndm('/add', { method: 'POST', headers: { 'X-NDM': '1', 'Content-Type': 'application/json' }, body });
      return r.status === 200 ? true : r.status === 409 ? 'page' : false;
    } catch {}
    if (i === 0) await launch();
    await new Promise(r => setTimeout(r, 250)); // up to 10 s for the app to start
  }
  return null;
}
const send = async (url, extra) => (await post(url, extra)) === true;

// The tab whose page started a download: the one showing its referrer, else one on the file's site.
async function pageTab(item) {
  const tabs = await chrome.tabs.query({}).catch(() => []);
  const origin = u => { try { return new URL(u).origin; } catch { return ''; } };
  const ref = item.referrer || '', site = origin(item.finalUrl || item.url);
  return tabs.find(t => ref && t.url === ref) || tabs.find(t => t.active && origin(t.url) === site)
    || tabs.find(t => origin(t.url) === site) || tabs.find(t => ref && origin(t.url) === origin(ref));
}

// A download the server won't give the app (a page that makes its files itself, as Telegram Web's service
// worker does): content.js fetches it inside that page and this passes the pieces to the app. Resolves true
// once the app has it in its list (the browser's copy can go), false to leave it to the browser.
async function feed(item) {
  const tab = await pageTab(item);
  if (!tab) return false;
  const url = item.finalUrl || item.url;
  let port;
  try {
    port = chrome.tabs.connect(tab.id, { name: 'ndm-feed', frameId: 0 });
  } catch {
    return false;
  }
  const head = { 'X-NDM': '1' };
  return new Promise(resolve => {
    let id = null, over = false;
    const finish = ok => {
      if (id === null || over) return;
      over = true;
      ndm(`/feed?op=end&id=${id}&ok=${ok ? 1 : 0}`, { method: 'POST', headers: head }).catch(() => {});
    };
    const timer = setTimeout(() => { port.disconnect(); resolve(false); }, 15000);
    port.onDisconnect.addListener(() => { clearTimeout(timer); finish(false); resolve(false); });
    port.onMessage.addListener(async m => {
      if (m.kind === 'no') {
        clearTimeout(timer);
        port.disconnect();
        return resolve(false);
      }
      if (m.kind === 'yes') {
        clearTimeout(timer);
        try {
          const body = JSON.stringify({ url, filename: (item.filename || '').split(/[\\/]/).pop(), size: m.size, mime: item.mime || '', referrer: item.referrer || '' });
          id = (await (await ndm('/feed?op=start', { method: 'POST', headers: { ...head, 'Content-Type': 'application/json' }, body })).json()).id;
        } catch {
          port.disconnect();
          return resolve(false);
        }
        resolve(true);
        return port.postMessage({ kind: 'go' });
      }
      if (m.kind === 'data') {
        const bin = Uint8Array.from(atob(m.data), c => c.charCodeAt(0));
        try {
          const r = await ndm(`/feed?op=data&id=${id}&at=${m.at}`, { method: 'POST', headers: { ...head, 'Content-Type': 'application/octet-stream' }, body: bin });
          if (!r.ok) throw new Error(String(r.status));
        } catch {
          finish(false);
          return port.disconnect();
        }
        return port.postMessage({ kind: 'more' });
      }
      if (m.kind === 'end' || m.kind === 'fail') {
        finish(m.kind === 'end');
        port.disconnect();
      }
    });
    port.postMessage({ kind: 'fetch', url, size: item.totalBytes > 0 ? item.totalBytes : 0 });
  });
}

// Special keys held in the page, reported by content.js.
let held = [];
const keysHeld = (k, which) => {
  const on = which === 'prevent'
    ? [['alt', k.alt_p], ['shift', k.shift_p], ['ctrl', k.ctrl_p], ['del', k.del_p]]
    : [['alt', k.alt_f], ['shift', k.shift_f], ['ctrl', k.ctrl_f], ['ins', k.ins_f]];
  const want = on.filter(x => x[1]).map(x => x[0]);
  return k[which] && want.length > 0 && want.every(x => held.includes(x));
};
const PAGE_LIKE = /^(text\/|image\/|application\/(x-)?javascript|application\/json)/i;

// While the app decides on a download the browser's download bubble stays hidden, the download is paused
// and its file name (Chrome's Save As dialog) held; the bubble comes back once nothing is waiting, so the
// downloads the browser keeps show in it as usual.
let deciding = 0;
const bubble = on => chrome.downloads.setUiOptions?.({ enabled: on }).catch(() => {});
bubble(true);
const web = item => /^https?:/i.test(item.finalUrl || item.url);
// downloads this extension hands back to the browser (the app couldn't take them): not offered again
const ours = new Set();

chrome.downloads.onCreated.addListener(item => {
  if (!web(item)) return;
  if (ours.delete(item.finalUrl || item.url) || ours.delete(item.url)) return;
  chrome.downloads.pause(item.id).catch(() => {});
  if (!chrome.downloads.onDeterminingFilename) handle(item, () => {}); // Firefox has no filename step
});
chrome.downloads.onDeterminingFilename?.addListener((item, suggest) => {
  if (!web(item) || ours.has('det:' + item.url)) return ours.delete('det:' + item.url) && false;
  handle(item, suggest);
  return true; // suggest() later: no Save As dialog meanwhile
});

async function handle(item, suggest) {
  if (deciding++ === 0) bubble(false);
  try {
    await decide(item, suggest);
  } finally {
    if (--deciding === 0) bubble(true);
  }
}

async function decide(item, suggest) {
  const k = (await refreshConfig()).keys;
  // the prevent keys (Options -> General -> Keys...) leave it to the browser
  const size = item.totalBytes > 0 ? item.totalBytes : item.fileSize > 0 ? item.fileSize : 0;
  const taken = k && keysHeld(k, 'prevent') ? false : await post(item.finalUrl || item.url, { referrer: item.referrer || '', filename: item.filename || '', mime: item.mime || '',
    size, check: true,
    // force keys without the link-click check: take over whatever starts while they are held
    force: !!k && !k.check_mouse && keysHeld(k, 'force') && !(k.skip_html && PAGE_LIKE.test(item.mime || '')) });
  // the server sends the app another file: the page hands it over, if it can
  if (taken === 'page' && await feed(item)) {
    await chrome.downloads.cancel(item.id).catch(() => {});
    return chrome.downloads.erase({ id: item.id });
  }
  if (taken !== true) {
    // declined (the app's file types and exceptions, or the server doesn't send the app the same file and the
    // page can't hand it over), or the app can't be reached: the browser downloads it as usual
    suggest();
    return chrome.downloads.resume(item.id).catch(() => {});
  }
  // taken by the app: the browser doesn't download it
  await chrome.downloads.cancel(item.id).catch(() => {});
  chrome.downloads.erase({ id: item.id });
}

// Sites that play videos in their own player: the app hands the page to yt-dlp (src/ytdlp.rs, SITES).
const VIDEO_SITES = ['youtube.com', 'youtu.be', 'vimeo.com', 'dailymotion.com', 'dai.ly', 'twitch.tv', 'tiktok.com', 'instagram.com',
  'facebook.com', 'fb.watch', 'x.com', 'twitter.com', 'reddit.com', 'v.redd.it', 'soundcloud.com', 'bilibili.com',
  'rumble.com', 'streamable.com', 'ok.ru', 'vk.com', 'bandcamp.com', 'mixcloud.com', 'douyin.com', 'kick.com',
  'threads.net', 'threads.com', 'pinterest.com', 'tumblr.com', 'linkedin.com', '9gag.com', 'imgur.com', 'weibo.com',
  'nicovideo.jp', 'bilibili.tv', 'snapchat.com', 'likee.video', 'coub.com', 'odysee.com', 'bitchute.com',
  'vkvideo.ru', 'rutube.ru', 'clips.twitch.tv'];

function isVideoSite(url) {
  try {
    const host = new URL(url).hostname.toLowerCase();
    return VIDEO_SITES.some(h => host === h || host.endsWith('.' + h));
  } catch {
    return false;
  }
}

// The app keeps a copy of this extension and refreshes it when the app is updated; when the app is newer
// than the extension running, the extension reloads itself from that copy (once per version, in case the
// browser loaded it from another folder).
async function selfUpdate(app) {
  const newer = (a, b) => {
    const x = String(a || '').split('.').map(Number), y = String(b || '').split('.').map(Number);
    for (let i = 0; i < 3; i++) if ((x[i] || 0) !== (y[i] || 0)) return (x[i] || 0) > (y[i] || 0);
    return false;
  };
  if (!newer(app, chrome.runtime.getManifest().version)) return;
  const { reloaded_for } = await chrome.storage.local.get('reloaded_for').catch(() => ({}));
  if (reloaded_for === app) return;
  await chrome.storage.local.set({ reloaded_for: app });
  chrome.runtime.reload();
}

// The app's Options decide the context menu items and the panel exceptions (370).
let config = { menu_dl: true, menu_all: true, panel_skip: '', keys: null }, configAt = 0, menus = '';
async function refreshConfig() {
  if (Date.now() - configAt < 10000) return config;
  configAt = Date.now();
  try {
    const r = await ndm('/config', { method: 'POST', headers: { 'X-NDM': '1' } });
    if (r.ok) config = await r.json();
  } catch {}
  selfUpdate(config.ext_version);
  const { panel_off = false } = await chrome.storage.local.get('panel_off').catch(() => ({}));
  if (menus === `${config.menu_dl}${config.menu_all}${panel_off}`) return config;
  menus = `${config.menu_dl}${config.menu_all}${panel_off}`;
  await chrome.contextMenus.removeAll();
  // the toolbar button's own menu: the download panel on or off, and the extension's options
  chrome.contextMenus.create({ id: 'ndm-panel', type: 'checkbox', checked: !panel_off, title: 'Show the download panel over videos', contexts: ['action'] });
  chrome.contextMenus.create({ id: 'ndm-options', title: 'Extension options...', contexts: ['action'] });
  // one item wherever the user right-clicks: a link or picture, a video or audio player (the file, or the
  // page for yt-dlp when the player streams it in pieces), or any web page ("page" applies only
  // where nothing more specific does)
  if (config.menu_dl) chrome.contextMenus.create({ id: 'ndm', title: 'Download with New Download Manager', contexts: ['link', 'image'] });
  if (config.menu_dl) chrome.contextMenus.create({ id: 'ndm-media', title: 'Download this video with New Download Manager', contexts: ['video'] });
  if (config.menu_dl) chrome.contextMenus.create({ id: 'ndm-audio', title: 'Download this audio with New Download Manager', contexts: ['audio'] });
  if (config.menu_all) chrome.contextMenus.create({ id: 'ndm-all', title: 'Download all links with New Download Manager', contexts: ['page', 'selection'] });
  if (config.menu_dl) chrome.contextMenus.create({ id: 'ndm-video', title: 'Download this video with New Download Manager', contexts: ['page'],
    documentUrlPatterns: ['http://*/*', 'https://*/*'] });
  return config;
}
chrome.runtime.onInstalled.addListener(refreshConfig);
chrome.runtime.onStartup.addListener(refreshConfig);

async function onMenu(info, tab) {
  if (info.menuItemId === 'ndm-panel') {
    await chrome.storage.local.set({ panel_off: !info.checked });
    menus = '';
    return;
  }
  if (info.menuItemId === 'ndm-options') {
    chrome.runtime.openOptionsPage();
    return;
  }
  // the right-clicked player's own page (a feed's address is no video's), from the frame that was clicked
  const clickedPage = async () => (tab ? await chrome.tabs.sendMessage(tab.id, { kind: 'video-page-of-click' }, { frameId: info.frameId || 0 }).catch(() => null) : null)
    || info.frameUrl || info.pageUrl || tab?.url || '';
  if (info.menuItemId === 'ndm-video') {
    // the page itself: the app's yt-dlp finds its video
    send(await clickedPage(), { referrer: '', force: true, video: true });
    return;
  }
  if (info.menuItemId === 'ndm-media' || info.menuItemId === 'ndm-audio') {
    // a player: its file when it plays one from an address, else (a stream in pieces, a video site) the page
    const page = info.frameUrl || info.pageUrl || tab?.url || '';
    if (/^https?:/i.test(info.srcUrl || '') && !isVideoSite(page)) send(info.srcUrl, { referrer: page, force: true });
    else send(await clickedPage(), { referrer: '', force: true, video: true });
    return;
  }
  if (info.menuItemId === 'ndm-all') {
    const links = await chrome.tabs.sendMessage(tab.id, { kind: 'links' }).catch(() => []);
    if (links?.length) send(tab.url, { referrer: tab.url, links, force: true });
    return;
  }
  send(info.linkUrl || info.srcUrl, { referrer: tab?.url || '', force: true });
}
chrome.contextMenus.onClicked.addListener(onMenu);

// ---- Video grabber: remember media responses per tab (session storage survives worker restarts) ----
const key = tabId => `media:${tabId}`;

async function mediaFor(tabId) {
  return (await chrome.storage.session.get(key(tabId)))[key(tabId)] || [];
}

function header(d, name) {
  return (d.responseHeaders || []).find(h => h.name.toLowerCase() === name)?.value || '';
}

chrome.webRequest.onResponseStarted.addListener(async d => {
  if (d.tabId < 0 || d.statusCode >= 400) return;
  const type = header(d, 'content-type').split(';')[0].trim().toLowerCase();
  const path = new URL(d.url).pathname;
  // HLS / DASH: the playlist is listed (the app fetches and joins its pieces); the pieces themselves aren't.
  // Known by the .m3u8/.mpd path or the Content-Type, like the app (stream::is_manifest, is_manifest_type).
  const stream = /\.(m3u8|mpd)$/i.test(path) || /mpegurl$|^application\/dash\+xml$/.test(type);
  if (!stream && (/mp2t/.test(type) || /\.(ts|m4s)$/i.test(path))) return;
  const c = await refreshConfig();
  const types = c.panel_types || ['MP4', 'WEBM', 'MP3', 'M4A', 'OGG', 'MKV', 'MOV', 'FLV'];
  const kind = stream ? 'MP4' : mediaType(type, path, types);
  if (!kind) return; // not a ticked type
  const total = header(d, 'content-range').split('/')[1];
  const size = Number(total && total !== '*' ? total : header(d, 'content-length')) || 0;
  const min = ((c.panel_min || {})[kind] || 0) * 1024;
  if (size && size < min) return;
  const list = await mediaFor(d.tabId);
  if (list.some(m => m.url === d.url)) return;
  list.push({ url: d.url, type: stream ? (/mpd$/i.test(path) || /dash/.test(type) ? 'dash' : 'hls') : type || path.split('.').pop(), size });
  await chrome.storage.session.set({ [key(d.tabId)]: list.slice(-20) });
  // "Don't capture downloads from web-players automatically" turned off: hand it over right away
  if (c.panel_nocap === false && !stream) send(d.url, { referrer: d.initiator || d.originUrl || '', mime: type, size, check: true }); // originUrl: Firefox
  chrome.tabs.sendMessage(d.tabId, { kind: 'media', count: list.length }).catch(() => {});
}, { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'other', 'object'] }, ['responseHeaders']);

chrome.tabs.onUpdated.addListener(async (tabId, change, tab) => {
  if (change.url) chrome.storage.session.remove(key(tabId));
  // the Site Grabber's manual login page: once the user has logged in, the app explores with these cookies
  if (change.status !== 'complete' || !/^https?:/i.test(tab.url || '')) return;
  const want = (await refreshConfig()).want_cookies;
  const host = new URL(tab.url).hostname.toLowerCase();
  const main = h => h.split('.').slice(-2).join('.');
  if (!want || (host !== want && !host.endsWith('.' + want) && main(host) !== main(want))) return;
  const cookies = (await chrome.cookies.getAll({ url: tab.url }).catch(() => [])).map(c => `${c.name}=${c.value}`).join('; ');
  ndm('/cookies', { method: 'POST', headers: { 'X-NDM': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ url: tab.url, cookies }) }).catch(() => {});
});
chrome.tabs.onRemoved.addListener(tabId => chrome.storage.session.remove(key(tabId)));

chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  const tab = sender.tab;
  if (!tab) return;
  if (msg.kind === 'keys') {
    held = msg.held || [];
    return;
  }
  if (msg.kind === 'config') {
    refreshConfig().then(reply);
    return true;
  }
  if (msg.kind === 'list') {
    mediaFor(tab.id).then(reply);
    return true;
  }
  if (msg.kind === 'video-page') {
    // the panel on a video site, or on any page whose video the browser can't hand over (a player that
    // streams it in pieces): the page itself, for the app's yt-dlp
    if (!msg.any && !isVideoSite(msg.url)) return reply(false);
    send(msg.url, { referrer: '', force: true, video: true }).then(ok => reply(ok ? 'sent' : 'off'));
    return true;
  }
  if (msg.kind === 'options') {
    chrome.runtime.openOptionsPage();
    return;
  }
  if (msg.kind === 'sel') {
    // the selected-links panel: like "Download all links", for the links in the selection
    send(tab.url, { referrer: tab.url, links: msg.links, force: true }).then(reply);
    return true;
  }
  if (msg.kind === 'download') {
    // a file the app can't get as the page did: the browser's own download takes it
    send(msg.url, { referrer: tab.url, filename: msg.filename || '', force: true, check: true, size: msg.size || 0, mime: msg.mime || '' }).then(ok => {
      if (!ok) {
        ours.add(msg.url);
        ours.add('det:' + msg.url);
        chrome.downloads.download({ url: msg.url, ...(msg.filename ? { filename: msg.filename } : {}) }).catch(() => {});
      }
      reply(ok);
    });
    return true;
  }
});
