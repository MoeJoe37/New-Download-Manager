// Downloads go to the app on 127.0.0.1; if it isn't running (or declines), the browser keeps them.
// The app listens on the first free one of these ports (Windows can reserve one for Hyper-V, WSL or Docker)
// and marks its replies "X-NDM: 1"; versions before 2.0 listen on 9614 only, without the mark.
const PORTS = [9614, 19614, 29614, 39614];
let port = null;
let finding = null;
const configRequest = { method: 'POST', headers: { 'X-NDM': '1' } };
const local = (p, path, init, ms) => fetch(`http://127.0.0.1:${p}${path}`, {
  ...init, redirect: 'error', signal: AbortSignal.timeout(ms),
});
const marked = r => r.headers.get('X-NDM') === '1';

function findApp() {
  if (finding) return finding;
  const candidates = port === null ? PORTS : [port, ...PORTS.filter(p => p !== port)];
  finding = Promise.any(candidates.map(async p => {
    const response = await local(p, '/config', configRequest, 1500);
    if (!response.ok || !marked(response)) throw new Error('Not NDM');
    return { p, response };
  })).then(found => {
    port = found.p;
    return found;
  }).finally(() => { finding = null; });
  return finding;
}

async function ndm(path, init) {
  if (path === '/config') return (await findApp()).response.clone();
  const p = port ?? (await findApp()).p;
  try {
    const response = await local(p, path, init, 20000);
    if (!marked(response)) throw new Error('Not NDM');
    return response;
  } catch (error) {
    if (port === p) port = null;
    // The desktop may have taken the request before the connection was lost.
    // Discovery can retry, but never replay a download or a file chunk on another port.
    throw Object.assign(new Error('NDM connection interrupted'), { sent: true, cause: error });
  }
}

// The app's native messaging host (ndm.exe itself, registered by the app at startup) starts the app when
// it isn't running.
let launching = null;
const launch = () => launching ??= chrome.runtime.sendNativeMessage('com.ndm.host', { kind: 'launch' })
  .catch(() => {}).finally(() => { launching = null; });

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
function webUrl(url) {
  try {
    const parsed = new URL(url);
    return /^https?:$/.test(parsed.protocol) && !parsed.username && !parsed.password;
  } catch { return false; }
}
async function post(url, extra) {
  if (!webUrl(url)) return false;
  const cookies = (await chrome.cookies.getAll({ url }).catch(() => [])).map(c => `${c.name}=${c.value}`).join('; ');
  const body = JSON.stringify({ url, cookies, proxy: await browserProxy(), ...extra });
  const until = Date.now() + 10000;
  for (let i = 0; Date.now() < until; i++) {
    try {
      const r = await ndm('/add', { method: 'POST', headers: { 'X-NDM': '1', 'Content-Type': 'application/json' }, body });
      return r.status === 200 ? true : r.status === 409 ? 'page' : false;
    } catch (error) {
      if (error.sent) return null;
    }
    if (i === 0) launch();
    await new Promise(r => setTimeout(r, 250)); // up to 10 s for the app to start
  }
  return null;
}
const send = async (url, extra) => (await post(url, extra)) === true;
