// Special keys: report Alt/Shift/Ctrl/Ins/Del to the background, and with
// "check for left mouse button" take a link clicked with the force keys held straight to the app.
(() => {
  const KEY = { Alt: 'alt', Shift: 'shift', Control: 'ctrl', Insert: 'ins', Delete: 'del' };
  const held = new Set();
  let keys = null;
  chrome.runtime.sendMessage({ kind: 'config' }).then(c => (keys = c && c.keys)).catch(() => {});
  const report = () => chrome.runtime.sendMessage({ kind: 'keys', held: [...held] }).catch(() => {});
  addEventListener('keydown', e => { const k = KEY[e.key]; if (k && !held.has(k)) { held.add(k); report(); } }, true);
  addEventListener('keyup', e => { const k = KEY[e.key]; if (k && held.delete(k)) report(); }, true);
  addEventListener('blur', () => { if (held.size) { held.clear(); report(); } });
  addEventListener('click', e => {
    if (!keys || !keys.force || !keys.check_mouse || e.button !== 0) return;
    const want = [['alt', keys.alt_f], ['shift', keys.shift_f], ['ctrl', keys.ctrl_f], ['ins', keys.ins_f]].filter(x => x[1]).map(x => x[0]);
    const a = e.target.closest && e.target.closest('a[href]');
    if (!a || !/^https?:/.test(a.href) || !want.length || !want.every(k => held.has(k))) return;
    if (keys.skip_html && /\.(html?|php|aspx?|jsp|jpe?g|png|gif|webp|svg|js|css)(\?|#|$)/i.test(new URL(a.href).pathname)) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    chrome.runtime.sendMessage({ kind: 'download', url: a.href });
  }, true);
})();

// A video's own page, for yt-dlp: on feeds (TikTok's For You, Instagram, X, Facebook, Reddit...) the address
// bar shows the feed, which yt-dlp can't take. The page comes from the post holding the player and nothing
// beyond it: never a link from elsewhere on the page (a notification, the next post), which would download
// another video. Shared by the download panel and the right-click menu.
const VIDEO_URL = [
  /tiktok\.com\/@[^/?#]+\/(video|photo)\/\d+/, /vm\.tiktok\.com\/\w+/, /instagram\.com\/([^/?#]+\/)?(p|reels?|tv)\/[\w-]+/,
  /(twitter|x)\.com\/[^/?#]+\/status\/\d+/, /facebook\.com\/(watch\/?\?v=|reel\/|[^/?#]+\/videos\/|share\/[rv]\/|video\.php)/, /fb\.watch\//,
  /reddit\.com\/r\/[^/?#]+\/comments\//, /youtube\.com\/(watch\?|shorts\/|live\/|embed\/)/, /youtu\.be\//, /vimeo\.com\/(\d+|video\/)/,
  /twitch\.tv\/(videos\/\d+|[^/?#]+\/clip\/|clip\/)/, /clips\.twitch\.tv\//, /dailymotion\.com\/video\//, /dai\.ly\//, /bilibili\.com\/video\//,
  /vk\.com\/(video|clip)-?\d/, /ok\.ru\/video\//, /rumble\.com\/v[\w-]+/, /streamable\.com\/\w+/, /soundcloud\.com\/[^/?#]+\/[^/?#]+/,
  /bandcamp\.com\/(track|album)\//, /mixcloud\.com\/[^/?#]+\/[^/?#]+/, /threads\.(net|com)\/@[^/?#]+\/post\//, /pinterest\.[a-z.]+\/pin\//,
  /linkedin\.com\/(posts|feed\/update)\//, /tumblr\.com\/.+\/\d+/, /9gag\.com\/gag\//, /imgur\.com\/(gallery|a)\//, /kick\.com\/[^/?#]+\/(videos|clips)\//,
];
const isVideoUrl = u => VIDEO_URL.some(p => p.test(u || ''));
// page furniture: a container holding it is the page, no longer the player's post
const FURNITURE = 'nav,[role="navigation"],main,[role="main"]';
// the containers around `el` that are still its post: up to (not including) the first that also holds another
// player or the page's furniture
function postOf(el) {
  const mine = el?.closest?.('video,audio') || el;
  const levels = [];
  for (let n = el; n && n.nodeType === 1 && n !== document.body && n !== document.documentElement && levels.length < 16; n = n.parentElement) {
    if (n.matches(FURNITURE) || n.querySelector(FURNITURE)) break;
    const other = [...n.querySelectorAll('video,audio')].some(v => v !== mine && !v.contains(mine) && !mine?.contains?.(v));
    if (other) break;
    levels.push(n);
  }
  return levels;
}
// TikTok's player carries its video's number in its id ("xgwrapper-0-7312345678901234567"): the page is
// tiktok.com/@<author>/video/<number> (yt-dlp takes it without the author too)
function tiktokPage(levels) {
  if (!/(^|\.)tiktok\.com$/.test(location.hostname)) return null;
  for (const n of levels) {
    const holder = [n, ...n.querySelectorAll('[id*="xgwrapper"],[id*="-video-"],[data-video-id]')].find(e => /\d{15,21}/.test((e.id || '') + (e.dataset?.videoId || '')));
    const id = holder && ((holder.id || '') + ' ' + (holder.dataset?.videoId || '')).match(/(\d{15,21})/)[1];
    if (id) {
      const post = levels[levels.length - 1];
      const author = [...post.querySelectorAll('a[href*="/@"]')].map(a => a.href.match(/tiktok\.com\/@([\w.-]+)\/?(?:[?#]|$)/)).find(Boolean);
      return `https://www.tiktok.com/@${author ? author[1] : ''}/video/${id}`;
    }
  }
  return null;
}
function videoPage(el) {
  if (isVideoUrl(location.href)) return location.href;
  const levels = postOf(el);
  const tok = tiktokPage(levels);
  if (tok) return tok;
  // from the player out: the first video link in its post (a link around the player, its time stamp...)
  for (const n of levels) {
    const own = n.closest('a[href]');
    if (own && isVideoUrl(own.href)) return own.href;
    const link = [...n.querySelectorAll('a[href]')].find(a => isVideoUrl(a.href));
    if (link) return link.href;
  }
  const meta = document.querySelector('meta[property="og:url"]')?.content || document.querySelector('link[rel="canonical"]')?.href;
  if (meta && isVideoUrl(meta)) return new URL(meta, location.href).href;
  return location.href;
}
// the element last right-clicked, for the menu's "Download this video" (the background asks for its page)
let rightClicked = null;
addEventListener('contextmenu', e => (rightClicked = e.target), true);

// "Download all links": every link on the page with its text, for the app's link list.
chrome.runtime.onMessage.addListener((m, _, reply) => {
  if (m.kind === 'video-page-of-click') {
    // the right-clicked player's own page (the menu asks the frame that was clicked)
    const media = rightClicked && (rightClicked.closest?.('video,audio') || [...document.querySelectorAll('video,audio')].find(v => v.contains(rightClicked) || rightClicked.contains?.(v)));
    return reply(videoPage(media || rightClicked || document.querySelector('video')));
  }
  if (m.kind === 'links' && window === window.top) {
    const seen = new Set();
    reply([...document.links].filter(a => /^https?:/.test(a.href) && !seen.has(a.href) && seen.add(a.href))
      .map(a => ({ url: a.href, text: (a.textContent || '').trim().slice(0, 200) })));
  }
});
