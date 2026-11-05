// Sites that play videos in their own player: the app hands the page to yt-dlp (src/ytdlp/mod.rs, SITES).
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

// The app's Options decide the context menu items and the panel exceptions.
let config = { menu_dl: true, menu_all: true, panel_skip: '', keys: null }, configAt = 0, menus = '';
let configPending;
function refreshConfig() {
  if (configPending) return configPending;
  configPending = readConfig().finally(() => { configPending = null; });
  return configPending;
}
async function readConfig() {
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
