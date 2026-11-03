// Real Chromium DOM/event test for the content script. The extension messaging boundary is
// supplied by the fixture; test-extension.mjs exercises the real background handlers.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFileSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { execFileSync } from 'node:child_process';

const require = createRequire(import.meta.url);
const playwright = process.env.NDM_TEST_PLAYWRIGHT || 'playwright';
const { chromium } = require(playwright);
const dir = mkdtempSync(join(tmpdir(), 'ndm-panel-'));
let browser;
try {
  const clip = join(dir, 'fixture.mp4');
  execFileSync('ffmpeg', ['-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=s=320x180:r=10:d=2',
    '-c:v', 'libx264', '-threads', '1', '-pix_fmt', 'yuv420p', clip]);
  const bytes = readFileSync(clip);
  browser = await chromium.launch({
    ...(process.env.NDM_TEST_CHROMIUM ? { executablePath: process.env.NDM_TEST_CHROMIUM } : {}),
  });
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.route('http://fixture.test/**', route => {
    if (route.request().url().endsWith('.mp4')) {
      return route.fulfill({ body: bytes, contentType: 'video/mp4' });
    }
    return route.fulfill({ body: '<!doctype html><title>NDM fixture</title><main></main>', contentType: 'text/html' });
  });
  await page.addInitScript(() => {
    // The production panel uses a closed shadow root. Expose it to selectors in this fixture.
    const attach = Element.prototype.attachShadow;
    Element.prototype.attachShadow = function (options) { return attach.call(this, { ...options, mode: 'open' }); };
    const listeners = [];
    window.ndmEvent = message => listeners.forEach(listener => listener(message, {}, () => {}));
    window.handoffs = [];
    window.chrome = {
      runtime: {
        id: 'ndm-test',
        onMessage: { addListener: fn => listeners.push(fn) },
        onConnect: { addListener() {} },
        sendMessage: async message => {
          if (message.kind === 'config') return { panel_drm: false };
          if (message.kind === 'media-list') return { items: [], types: ['MP4'], minimums: {} };
          if (message.kind === 'download') { window.handoffs.push(message); return true; }
          return false;
        },
      },
      storage: { local: { get: async () => ({}) }, onChanged: { addListener() {} } },
    };
  });
  await page.goto('http://fixture.test/index');
  await page.addScriptTag({ path: new URL('../extension/content.js', import.meta.url).pathname });
  const panel = page.getByText('Download this video', { exact: true });
  const insert = async src => page.evaluate(async src => {
    const video = document.createElement('video');
    video.width = 320;
    video.height = 180;
    video.muted = true;
    video.src = src;
    document.querySelector('main').append(video);
    await video.play();
  }, src);
  await insert('/first.mp4');
  await panel.waitFor({ state: 'visible' });
  await panel.click();
  await page.waitForFunction(() => handoffs.length === 1);
  assert.equal(await page.evaluate(() => handoffs[0].url), 'http://fixture.test/first.mp4');

  await page.evaluate(async () => {
    history.pushState({}, '', '/next-video');
    ndmEvent({ kind: 'ndm-navigation' });
    const video = document.querySelector('video');
    video.src = '/second.mp4';
    await video.play();
  });
  await panel.waitFor({ state: 'visible' });
  await panel.click();
  await page.waitForFunction(() => handoffs.length === 2);
  assert.equal(await page.evaluate(() => handoffs[1].url), 'http://fixture.test/second.mp4');
  assert.equal(await panel.count(), 1);

  await page.evaluate(() => document.querySelector('video').remove());
  await panel.waitFor({ state: 'hidden' });
  await insert('/replacement.mp4');
  await panel.waitFor({ state: 'visible' });
  await page.evaluate(() => document.querySelector('video').dispatchEvent(new Event('encrypted')));
  await panel.waitFor({ state: 'hidden' });
  assert.deepEqual(errors, []);
  console.log('Chromium: dynamic video, SPA navigation, changed source, removed player, protected player and one panel');
} finally {
  await browser?.close();
  rmSync(dir, { recursive: true, force: true });
}
