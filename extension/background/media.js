// The file type of a media response, as named in the app's download panel list.
const MIME_TYPE = { 'audio/mpeg': 'MP3', 'video/mpeg': 'MPG', 'video/x-flv': 'FLV', 'video/quicktime': 'MOV', 'video/x-matroska': 'MKV',
  'video/x-ms-wmv': 'WMV', 'audio/x-ms-wma': 'WMA', 'video/x-ms-asf': 'ASF', 'video/3gpp': '3GP', 'audio/mp4': 'M4A', 'audio/x-m4a': 'M4A',
  'video/mp2t': 'TS', 'audio/aac': 'AAC', 'video/x-msvideo': 'AVI', 'audio/wav': 'WAV', 'audio/x-wav': 'WAV', 'audio/vnd.wave': 'WAV' };
function mediaType(mime, path, types) {
  const ext = ((path.match(/\.(\w{2,5})$/) || [])[1] || '').toUpperCase();
  const byMime = MIME_TYPE[mime] || (/^(video|audio)\//.test(mime) ? mime.split('/')[1].toUpperCase() : '');
  return [byMime, ext].find(t => t && types.includes(t)) || '';
}

// Store a bounded list per tab, with frame/document identity. Serialize writes: two responses
// arriving together must not read and overwrite the same old list.
const key = tabId => `media:${tabId}`;
const mediaWrites = new Map();
const mediaGeneration = new Map();
const DEFAULT_TYPES = ['MP4', 'WEBM', 'MP3', 'M4A', 'OGG', 'MKV', 'MOV', 'FLV', 'TS', 'AAC'];

function updateMedia(tabId, work) {
  const pending = (mediaWrites.get(tabId) || Promise.resolve()).catch(() => {}).then(work);
  mediaWrites.set(tabId, pending);
  return pending.finally(() => {
    if (mediaWrites.get(tabId) === pending) mediaWrites.delete(tabId);
  });
}
async function storedMedia(tabId) {
  return (await chrome.storage.session.get(key(tabId)).catch(() => ({})))[key(tabId)] || [];
}

function visibleMedia(media, settings) {
  const min = ((settings.panel_min || {})[media.ext] || 0) * 1024;
  return (media.stream || (settings.panel_types || DEFAULT_TYPES).includes(media.ext)) && (!media.size || media.size >= min);
}
async function mediaFor(sender) {
  const tabId = sender.tab.id;
  await mediaWrites.get(tabId)?.catch(() => {});
  const settings = await refreshConfig();
  const items = await storedMedia(tabId);
  return {
    types: settings.panel_types || DEFAULT_TYPES,
    minimums: settings.panel_min || {},
    items: items.filter(item => item.frameId === (sender.frameId || 0) &&
      (!item.documentId || !sender.documentId || item.documentId === sender.documentId) &&
      (!item.documentUrl || !sender.url || item.documentUrl.split('#')[0] === sender.url.split('#')[0]) &&
      visibleMedia(item, settings)),
  };
}

function header(d, name) {
  return (d.responseHeaders || []).find(h => h.name.toLowerCase() === name)?.value || '';
}

function detectedMedia(d) {
  if (d.tabId < 0 || ![200, 206].includes(d.statusCode) || !webUrl(d.url)) return null;
  const url = new URL(d.url);
  const path = url.pathname;
  if (/(^|\.)(doubleclick\.net|googlesyndication\.com)$/i.test(url.hostname) ||
      /\/(ads?|preroll|midroll|vast|vpaid)(?:\/|$)/i.test(path)) return null;
  const mime = header(d, 'content-type').split(';')[0].trim().toLowerCase();
  const stream = /\.mpd$/i.test(path) || mime === 'application/dash+xml' ? 'dash'
    : /\.m3u8$/i.test(path) || /mpegurl$/.test(mime) ? 'hls' : '';
  if ((/^(text|image)\//.test(mime) && !(stream && mime === 'text/plain')) || /(?:javascript|json)$/.test(mime)) return null;
  if (!stream && /\.(m4s|cmfv|cmfa)$/i.test(path)) return null;
  const ext = stream ? '' : mediaType(mime, path, [...new Set([...DEFAULT_TYPES, ...(config.panel_types || [])])]);
  if (!stream && !ext) return null;
  // Segment fetches typically use XHR and a .ts name. Native player TS and standalone
  // downloadable TS files are valid sources, but avoid offering each HLS fragment.
  if (ext === 'TS' && d.type !== 'media' && !/^attachment/i.test(header(d, 'content-disposition'))) return null;
  const range = header(d, 'content-range').split('/')[1];
  const size = Number(range && range !== '*' ? range : d.statusCode === 200 ? header(d, 'content-length') : 0) || 0;
  url.hash = '';
  return { url: url.href, mime, ext, stream, size, frameId: d.frameId || 0, documentId: d.documentId,
    documentUrl: d.documentUrl, etag: header(d, 'etag') };
}

async function recordMedia(d) {
  const item = detectedMedia(d);
  if (!item) return;
  const generation = mediaGeneration.get(d.tabId);
  const changed = await updateMedia(d.tabId, async () => {
    if (generation !== mediaGeneration.get(d.tabId)) return false;
    const list = await storedMedia(d.tabId);
    const same = old => old.frameId === item.frameId && old.documentId === item.documentId &&
      (old.url === item.url || item.etag && old.etag === item.etag && old.size === item.size &&
        old.mime === item.mime && old.url.split('?')[0] === item.url.split('?')[0]);
    const previous = list.find(same);
    if (previous?.url === item.url && previous.size >= item.size) return false;
    const next = list.filter(old => !same(old));
    next.push(item);
    await chrome.storage.session.set({ [key(d.tabId)]: next.slice(-32) });
    return !previous;
  });
  if (!changed || generation !== mediaGeneration.get(d.tabId)) return;
  const settings = await refreshConfig();
  if (settings.panel_nocap === false && !item.stream && visibleMedia(item, settings)) {
    send(item.url, { referrer: d.documentUrl || d.initiator || d.originUrl || '', mime: item.mime, size: item.size, check: true });
  }
  chrome.tabs.sendMessage(d.tabId, { kind: 'media' }, { frameId: item.frameId }).catch(() => {});
}

function clearMedia(tabId) {
  mediaGeneration.set(tabId, Symbol());
  return updateMedia(tabId, () => chrome.storage.session.remove(key(tabId))).catch(() => {});
}
