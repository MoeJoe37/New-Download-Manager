// The tab whose page started a download: the one showing its referrer, else one on the file's site.
async function pageTab(item) {
  const tabs = await chrome.tabs.query({}).catch(() => []);
  const origin = u => { try { return new URL(u).origin; } catch { return ''; } };
  const ref = item.referrer || '', site = origin(item.finalUrl || item.url);
  return tabs.find(t => ref && t.url === ref) || tabs.find(t => t.active && origin(t.url) === site)
    || tabs.find(t => origin(t.url) === site) || tabs.find(t => ref && origin(t.url) === origin(ref));
}

// A download the server won't give the app (a page that makes its files itself, as Telegram Web's service
// worker does): the page feed fetches it inside that page. This passes the pieces to the app and resolves
// true once the app has it in its list (the browser's copy can go), false to leave it to the browser.
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
const web = item => /^https?:/i.test(item.finalUrl || item.url);
// downloads this extension hands back to the browser (the app couldn't take them): not offered again
const ours = new Set();

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
