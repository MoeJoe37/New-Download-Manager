// Load the shipped manifest and exercise messaging across all three browser contexts.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.NDM_TEST_PLAYWRIGHT || 'playwright');
const extension = fileURLToPath(new URL('../extension', import.meta.url));
const context = await chromium.launchPersistentContext('', {
  channel: 'chromium',
  headless: true,
  args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
});

try {
  const worker = context.serviceWorkers()[0] ?? await context.waitForEvent('serviceworker', { timeout: 15000 });
  assert.match(worker.url(), /^chrome-extension:\/\/[^/]+\/background\.js$/);
  assert.equal(await worker.evaluate(() =>
    chrome.runtime.onMessage.hasListeners() && chrome.downloads.onCreated.hasListeners()), true);
  const page = context.pages()[0];
  const url = 'http://localhost/ndm-fixture';
  const file = `${url}/file.zip`;
  await page.route(`${url}**`, route => route.request().url() === file
    ? route.fulfill({ body: 'fixture bytes', contentType: 'application/zip' })
    : route.fulfill({ body: '<a href="ndm-fixture/file.zip">Fixture</a>', contentType: 'text/html' }));
  await page.goto(url, { waitUntil: 'networkidle' });

  const links = await worker.evaluate(async url => {
    const [tab] = await chrome.tabs.query({ url });
    return chrome.tabs.sendMessage(tab.id, { kind: 'links' });
  }, url);
  assert.deepEqual(links, [{ url: file, text: 'Fixture' }]);

  const data = await worker.evaluate(async ({ url, file }) => {
    const [tab] = await chrome.tabs.query({ url });
    const port = chrome.tabs.connect(tab.id, { name: 'ndm-feed' });
    try {
      return await new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('Page feed did not finish')), 10000);
        const finish = () => clearTimeout(timer);
        port.onDisconnect.addListener(() => {
          finish();
          reject(new Error(chrome.runtime.lastError?.message || 'Page feed disconnected'));
        });
        let bytes = '';
        port.onMessage.addListener(message => {
          if (message.kind === 'yes') port.postMessage({ kind: 'go' });
          else if (message.kind === 'data') {
            bytes += atob(message.data);
            port.postMessage({ kind: 'more' });
          } else {
            finish();
            if (message.kind === 'end') resolve(bytes);
            else reject(new Error(`Page feed: ${message.kind} ${message.why || ''}`));
          }
        });
        port.postMessage({ kind: 'fetch', url: file });
      });
    } finally {
      port.disconnect();
    }
  }, { url, file });
  assert.equal(data, 'fixture bytes');
  console.log('Chromium: packaged service worker, isolated content script and MAIN-world page feed');
} finally {
  await context.close();
}
