// Downloads go to the app at 127.0.0.1:9614; if it isn't running (or declines), the browser keeps them.
const API = 'http://127.0.0.1:9614/add';
const CONFIG = 'http://127.0.0.1:9614/config';
// The file type of a media response, as named in the app's download panel list (NDM template 340).
const MIME_TYPE = { 'audio/mpeg': 'MP3', 'video/mpeg': 'MPG', 'video/x-flv': 'FLV', 'video/quicktime': 'MOV', 'video/x-matroska': 'MKV',
  'video/x-ms-wmv': 'WMV', 'audio/x-ms-wma': 'WMA', 'video/x-ms-asf': 'ASF', 'video/3gpp': '3GP', 'audio/mp4': 'M4A', 'audio/x-m4a': 'M4A',
  'video/x-msvideo': 'AVI', 'audio/wav': 'WAV', 'audio/x-wav': 'WAV', 'audio/vnd.wave': 'WAV' };
function mediaType(mime, path, types) {
  const ext = ((path.match(/\.(\w{2,5})$/) || [])[1] || '').toUpperCase();
  const byMime = MIME_TYPE[mime] || (/^(video|audio)\//.test(mime) ? mime.split('/')[1].toUpperCase() : '');
  return [ext, byMime].find(t => t && types.includes(t)) || '';
}

async function send(url, extra) {
  const cookies = (await chrome.cookies.getAll({ url }).catch(() => [])).map(c => `${c.name}=${c.value}`).join('; ');
  try {
    const r = await fetch(API, {
      method: 'POST',
      headers: { 'X-NDM': '1', 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, cookies, ...extra }),
    });
    return r.status === 200;
  } catch {
    return false;
  }
}

// Special keys held in the page (Options -> General -> Keys..., NDM template 229), reported by content.js.
let held = [];
const keysHeld = (k, which) => {
  const on = which === 'prevent'
    ? [['alt', k.alt_p], ['shift', k.shift_p], ['ctrl', k.ctrl_p], ['del', k.del_p]]
    : [['alt', k.alt_f], ['shift', k.shift_f], ['ctrl', k.ctrl_f], ['ins', k.ins_f]];
  const want = on.filter(x => x[1]).map(x => x[0]);
  return k[which] && want.length > 0 && want.every(x => held.includes(x));
};
const PAGE_LIKE = /^(text\/|image\/|application\/(x-)?javascript|application\/json)/i;

chrome.downloads.onCreated.addListener(async item => {
  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url)) return;
  const k = (await refreshConfig()).keys;
  if (k && keysHeld(k, 'prevent')) return; // the browser keeps it
  // force keys without the link-click check: take over whatever starts while they are held
  const force = !!k && !k.check_mouse && keysHeld(k, 'force') && !(k.skip_html && PAGE_LIKE.test(item.mime || ''));
  if (await send(url, { referrer: item.referrer || '', filename: item.filename || '', mime: item.mime || '', force })) {
    await chrome.downloads.cancel(item.id).catch(() => {});
    chrome.downloads.erase({ id: item.id });
  }
});

// The app's Options decide the context menu items (NDM template 334) and the panel exceptions (370).
let config = { menu_dl: true, menu_all: true, panel_skip: '', keys: null }, configAt = 0, menus = '';
async function refreshConfig() {
  if (Date.now() - configAt < 10000) return config;
  configAt = Date.now();
  try {
    const r = await fetch(CONFIG, { method: 'POST', headers: { 'X-NDM': '1' } });
    if (r.ok) config = await r.json();
  } catch {}
  if (menus === `${config.menu_dl}${config.menu_all}`) return config;
  menus = `${config.menu_dl}${config.menu_all}`;
  await chrome.contextMenus.removeAll();
  if (config.menu_dl) chrome.contextMenus.create({ id: 'ndm', title: 'Download with New Download Manager', contexts: ['link', 'video', 'audio', 'image'] });
  if (config.menu_all) chrome.contextMenus.create({ id: 'ndm-all', title: 'Download all links with New Download Manager', contexts: ['page', 'selection'] });
  return config;
}
chrome.runtime.onInstalled.addListener(refreshConfig);
chrome.runtime.onStartup.addListener(refreshConfig);

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId === 'ndm-all') {
    const links = await chrome.tabs.sendMessage(tab.id, { kind: 'links' }).catch(() => []);
    if (links?.length) send(tab.url, { referrer: tab.url, links, force: true });
    return;
  }
  send(info.linkUrl || info.srcUrl, { referrer: tab?.url || '', force: true });
});

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
  if (c.panel_nocap === false && !stream) send(d.url, { referrer: d.initiator || d.originUrl || '', mime: type }); // originUrl: Firefox
  chrome.tabs.sendMessage(d.tabId, { kind: 'media', count: list.length }).catch(() => {});
}, { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'other', 'object'] }, ['responseHeaders']);

chrome.tabs.onUpdated.addListener((tabId, change) => {
  if (change.url) chrome.storage.session.remove(key(tabId));
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
  if (msg.kind === 'sel') {
    // the selected-links panel: like "Download all links", for the links in the selection
    send(tab.url, { referrer: tab.url, links: msg.links, force: true }).then(reply);
    return true;
  }
  if (msg.kind === 'download') {
    send(msg.url, { referrer: tab.url, filename: msg.filename || '', force: true }).then(reply);
    return true;
  }
});
