// Chrome loads the shared scripts in its worker. Firefox lists them before this entry in the manifest.
if (typeof importScripts === "function") {
  importScripts('background/desktop.js', 'background/downloads.js', 'background/menus.js', 'background/media.js');
}

chrome.action.onClicked.addListener(launch);

bubble(true);

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

chrome.runtime.onInstalled.addListener(refreshConfig);
chrome.runtime.onStartup.addListener(refreshConfig);

chrome.contextMenus.onClicked.addListener(onMenu);

chrome.webRequest.onResponseStarted.addListener(d => {
  recordMedia(d).catch(() => {});
}, { urls: ['<all_urls>'], types: ['media', 'xmlhttprequest', 'other', 'object'] }, ['responseHeaders']);

chrome.tabs.onUpdated.addListener(async (tabId, change, tab) => {
  if (change.url || change.status === 'loading') {
    await clearMedia(tabId);
    chrome.tabs.sendMessage(tabId, { kind: 'ndm-navigation' }).catch(() => {});
  }
  // the Site Grabber's manual login page: once the user has logged in, the app explores with these cookies
  if (change.status !== 'complete' || !/^https?:/i.test(tab.url || '')) return;
  const want = (await refreshConfig()).want_cookies;
  const host = new URL(tab.url).hostname.toLowerCase();
  const main = h => h.split('.').slice(-2).join('.');
  if (!want || (host !== want && !host.endsWith('.' + want) && main(host) !== main(want))) return;
  const cookies = (await chrome.cookies.getAll({ url: tab.url }).catch(() => [])).map(c => `${c.name}=${c.value}`).join('; ');
  ndm('/cookies', { method: 'POST', headers: { 'X-NDM': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ url: tab.url, cookies }) }).catch(() => {});
});
chrome.tabs.onRemoved.addListener(tabId => clearMedia(tabId).finally(() => mediaGeneration.delete(tabId)));

chrome.runtime.onMessage.addListener((msg, sender, reply) => {
  const tab = sender.tab;
  if (!tab || !msg || typeof msg.kind !== 'string' || (sender.id && sender.id !== chrome.runtime.id)) return;
  if (msg.kind === 'keys') {
    held = msg.held || [];
    return;
  }
  if (msg.kind === 'config') {
    refreshConfig().then(reply);
    return true;
  }
  if (msg.kind === 'media-list') {
    mediaFor(sender).then(reply, () => reply({ items: [], types: DEFAULT_TYPES }));
    return true;
  }
  if (msg.kind === 'list') {
    mediaFor(sender).then(result => reply(result.items.map(item => ({ ...item, type: item.stream || item.mime || item.ext }))), () => reply([]));
    return true;
  }
  if (msg.kind === 'video-page') {
    if (!webUrl(msg.url)) return reply(false);
    // the panel on a video site, or on any page whose video the browser can't hand over (a player that
    // streams it in pieces): the page itself, for the app's yt-dlp
    if (!msg.any && !isVideoSite(msg.url)) return reply(false);
    send(msg.url, { referrer: '', force: true, video: true }).then(ok => reply(ok ? 'sent' : 'off'));
    return true;
  }
  if (msg.kind === 'formats') {
    if (!webUrl(msg.url) || !msg.stream && msg.mime !== 'video/mp2t') return reply([]);
    (async () => {
      const cookies = (await chrome.cookies.getAll({ url: msg.url }).catch(() => [])).map(c => `${c.name}=${c.value}`).join('; ');
      const response = await ndm('/formats', { method: 'POST', headers: { 'X-NDM': '1', 'Content-Type': 'application/json' },
        body: JSON.stringify({ url: msg.url, referrer: sender.url || tab.url, cookies, stream: !!msg.stream, mime: msg.mime || '', force: true }) });
      return response.ok ? response.json() : [];
    })().then(reply, () => reply([]));
    return true;
  }
  if (msg.kind === 'options') {
    chrome.runtime.openOptionsPage();
    return;
  }
  if (msg.kind === 'sel') {
    // the selected-links panel: like "Download all links", for the links in the selection
    const links = Array.isArray(msg.links) ? msg.links.slice(0, 5000).filter(link => webUrl(link?.url)) : [];
    if (!links.length) return reply(false);
    send(tab.url, { referrer: tab.url, links, force: true }).then(reply);
    return true;
  }
  if (msg.kind === 'download') {
    // a file the app can't get as the page did: the browser's own download takes it
    if (!webUrl(msg.url)) return reply(false);
    const stream = msg.stream === 'hls' || msg.stream === 'dash';
    send(msg.url, { referrer: sender.url || tab.url, filename: msg.filename || '', force: true,
      check: !stream, size: msg.size || 0, mime: msg.mime || '', stream,
      output: (stream || msg.mime === 'video/mp2t') && ['mp4', 'ts', 'mkv', 'webm'].includes(msg.output) ? msg.output : '',
      variant: stream && Number.isSafeInteger(msg.variant) && msg.variant >= 0 ? msg.variant : null }).then(ok => {
      if (!ok && !stream && !msg.output) {
        ours.add(msg.url);
        ours.add('det:' + msg.url);
        chrome.downloads.download({ url: msg.url, ...(msg.filename ? { filename: msg.filename } : {}) }).catch(() => {
          ours.delete(msg.url);
          ours.delete('det:' + msg.url);
        });
      }
      reply(ok);
    });
    return true;
  }
});
