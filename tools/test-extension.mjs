// Run with node tools/test-extension.mjs. Exercise the real background script without a browser.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const extension = new URL('../extension/', import.meta.url);
const manifest = JSON.parse(readFileSync(new URL('manifest.json', extension), 'utf8'));
assert.equal(manifest.background.scripts.at(-1), manifest.background.service_worker);

async function testBackground(browser) {
  const listeners = [];
  const event = name => ({ addListener() { listeners.push(name); } });
  const menus = [], added = [];
  const session = new Map();
  let settings = { menu_dl: true, menu_all: true, panel_types: ['MP4', 'TS', 'MKV'] };
  let activePort = 9614;
  let interrupted = false;
  const requests = [];
  const chrome = {
    action: { onClicked: event('action') },
    runtime: {
      onInstalled: event('onInstalled'), onStartup: event('onStartup'), onMessage: event('onMessage'),
      getManifest: () => manifest,
      sendNativeMessage: async () => ({}),
    },
    cookies: { getAll: async () => [{ name: 'session', value: 'signed-in' }] },
    storage: {
      local: { get: async () => ({}), set: async () => {} },
      session: {
        get: async key => ({ [key]: session.get(key) }),
        set: async update => { for (const [key, value] of Object.entries(update)) session.set(key, value); },
        remove: async key => { session.delete(key); },
      },
    },
    contextMenus: {
      removeAll: async () => {}, create: menu => menus.push(menu), onClicked: event('onClicked'),
    },
    downloads: { setUiOptions: async () => {}, onCreated: event('onCreated'),
      ...(browser === 'Chrome' ? { onDeterminingFilename: event('onDeterminingFilename') } : {}) },
    tabs: { onUpdated: event('onUpdated'), onRemoved: event('onRemoved'), sendMessage: async () => null },
    webRequest: { onResponseStarted: event('onResponseStarted') },
  };
  const context = vm.createContext({ chrome, URL, AbortSignal, setTimeout, clearTimeout, fetch: async (url, init) => {
    requests.push(url);
    if (Number(new URL(url).port) !== activePort) return new Response('{}');
    if (url.endsWith('/add')) {
      if (interrupted) throw new Error('lost response');
      added.push(JSON.parse(init.body));
    }
    return new Response(JSON.stringify(settings), { headers: { 'X-NDM': '1' } });
  } });
  const run = file => vm.runInContext(readFileSync(new URL(file, extension), 'utf8'), context, { filename: file });
  if (browser === 'Chrome') {
    context.importScripts = (...files) => {
      assert.deepEqual(files, manifest.background.scripts.slice(0, -1));
      files.forEach(run);
    };
    run(manifest.background.service_worker);
  } else {
    manifest.background.scripts.forEach(run);
  }
  assert.deepEqual(listeners, [
    'action', 'onCreated', ...(browser === 'Chrome' ? ['onDeterminingFilename'] : []),
    'onInstalled', 'onStartup', 'onClicked', 'onResponseStarted', 'onUpdated', 'onRemoved', 'onMessage',
  ]);
  await context.refreshConfig();
  const menu = menus.find(menu => menu.id === 'ndm-video');
  assert.deepEqual([...menu.documentUrlPatterns], ['http://*/*', 'https://*/*']);

  // A yt-dlp site outside VIDEO_SITES still reaches the app explicitly as a video page.
  const url = 'https://www.arte.tv/en/videos/123/example/';
  await context.onMenu({ menuItemId: 'ndm-video', pageUrl: url }, { id: 1, url });
  await new Promise(setImmediate);
  assert.equal(added.length, 1);
  assert.equal(added[0].url, url);
  assert.equal(added[0].video, true);
  assert.equal(added[0].force, true);
  assert.equal(added[0].cookies, 'session=signed-in');
  console.log(browser + ' extension: any-site video menu sends the page and its session to NDM');

  const response = (url, mime, frameId = 0, extras = {}) => ({
    url, tabId: 1, frameId, documentId: frameId ? 'child' : 'main', statusCode: 200, type: 'media',
    responseHeaders: [{ name: 'Content-Type', value: mime }, { name: 'Content-Length', value: '5000' }], ...extras,
  });
  const page = { tab: { id: 1 }, frameId: 0, documentId: 'main', url };
  const mp4 = response('https://media.example/video?signature=1', 'video/mp4');
  const ts = response('https://media.example/movie.ts', 'video/mp2t');
  const mkv = response('https://media.example/another', 'video/x-matroska');
  const hls = response('https://media.example/master', 'application/x-mpegurl', 2);
  const unrelated = [
    response('https://media.example/chunk.ts', 'video/mp2t', 0, { type: 'xmlhttprequest' }),
    response('https://media.example/ad/preroll.mp4', 'video/mp4'),
    response('https://media.example/thumbnail.mp4', 'image/jpeg'),
  ];
  await Promise.all([mp4, ts, mkv, hls, ...unrelated].map(item => context.recordMedia(item)));
  assert.deepEqual(Array.from((await context.mediaFor(page)).items, m => m.ext), ['MP4', 'TS', 'MKV']);
  assert.deepEqual(Array.from((await context.mediaFor({ ...page, frameId: 2, documentId: 'child' })).items, m => m.stream), ['hls']);
  await context.recordMedia(mp4);
  assert.equal((await context.mediaFor(page)).items.length, 3);
  settings = { ...settings, panel_types: ['MP4', 'MKV'] };
  vm.runInContext('configAt = 0', context);
  assert.deepEqual(Array.from((await context.mediaFor(page)).items, m => m.ext), ['MP4', 'MKV']);
  const inflight = context.recordMedia(response('https://media.example/old', 'video/mp4'));
  await context.clearMedia(1);
  await inflight;
  assert.equal((await context.mediaFor(page)).items.length, 0);

  activePort = 19614;
  interrupted = true;
  await context.refreshConfig();
  const before = requests.filter(url => url.endsWith('/add')).length;
  await context.post(url, { video: true });
  assert.equal(requests.filter(url => url.endsWith('/add')).length, before + 1, 'a possibly accepted request is not replayed');
  interrupted = false;
  assert.equal(await context.post(url, { video: true }), true, 'a later request discovers the replacement app');
  assert.match(requests.at(-1), /19614\/add$/);
  console.log(browser + ' extension: media type, frame, format filter, navigation and reconnect regressions passed');
}

for (const browser of ['Chrome', 'Firefox']) await testBackground(browser);
