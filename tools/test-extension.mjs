// Run with node tools/test-extension.mjs. Exercise the real background script without a browser.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const event = () => ({ addListener() {} });
const menus = [], added = [];
const chrome = {
  action: { onClicked: event() },
  runtime: {
    onInstalled: event(), onStartup: event(), onMessage: event(),
    getManifest: () => ({ version: '2.7.5' }),
    sendNativeMessage: async () => ({}),
  },
  cookies: { getAll: async () => [{ name: 'session', value: 'signed-in' }] },
  storage: {
    local: { get: async () => ({}), set: async () => {} },
    session: { get: async () => ({}) },
  },
  contextMenus: {
    removeAll: async () => {}, create: menu => menus.push(menu), onClicked: event(),
  },
  downloads: { setUiOptions: async () => {}, onCreated: event(), onDeterminingFilename: event() },
  tabs: { onUpdated: event(), onRemoved: event(), sendMessage: async () => null },
  webRequest: { onResponseStarted: event() },
};
const context = vm.createContext({ chrome, URL, setTimeout, clearTimeout, fetch: async (url, init) => {
  if (url.endsWith('/add')) added.push(JSON.parse(init.body));
  return { status: 200, ok: true, headers: { get: () => '1' }, json: async () => ({ menu_dl: true, menu_all: true }) };
} });
vm.runInContext(readFileSync(new URL('../extension/background.js', import.meta.url), 'utf8'), context);
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
console.log('Extension: any-site video menu sends the page and its session to NDM');
