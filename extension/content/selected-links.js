// Download panel for selected links: selecting text that holds links shows a button
// that hands those links to the app's link list.
(async () => {
  if (window !== window.top || window.__ndmSel) return;
  window.__ndmSel = true;
  const { sel_mode = 0, sel_sites = [], sel_mini = false } = (await chrome.runtime.sendMessage({ kind: 'config' }).catch(() => null)) || {};
  const site = location.hostname.toLowerCase();
  if (sel_mode === 1 || (sel_mode === 2 && !sel_sites.some(s => site === s || site.endsWith('.' + s)))) return;
  const host = document.createElement('div');
  host.style.cssText = 'position:absolute;z-index:2147483647;display:none';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = `<style>.b{font:12px Tahoma,Segoe UI,sans-serif;color:#fff;cursor:pointer;white-space:nowrap;user-select:none;
    background:linear-gradient(#4a90e2,#1f5fb8);border:1px solid #174a8f;border-radius:3px;padding:3px 8px;box-shadow:0 1px 4px rgba(0,0,0,.45)}
    .b:hover{background:linear-gradient(#5ea0ee,#2a6ccb)}</style><div class="b"></div>`;
  const b = root.querySelector('.b');
  let links = [];
  const hide = () => (host.style.display = 'none');
  b.addEventListener('mousedown', e => e.preventDefault()); // keep the selection
  b.addEventListener('click', () => {
    chrome.runtime.sendMessage({ kind: 'sel', links });
    hide();
  });
  document.addEventListener('mouseup', e => {
    if (e.composedPath().includes(host)) return;
    setTimeout(() => {
      const s = getSelection();
      const seen = new Set();
      links = s && !s.isCollapsed ? [...document.links].filter(a => /^https?:/.test(a.href) && s.containsNode(a, true) && !seen.has(a.href) && seen.add(a.href))
        .map(a => ({ url: a.href, text: (a.textContent || '').trim().slice(0, 200) })) : [];
      if (!links.length) return hide();
      b.textContent = sel_mini ? '↓' : `↓ Download selected links (${links.length})`;
      if (!host.isConnected) document.documentElement.appendChild(host);
      host.style.display = 'block';
      host.style.left = `${scrollX + e.clientX + 8}px`;
      host.style.top = `${scrollY + e.clientY + 12}px`;
    }, 0);
  }, true);
  document.addEventListener('mousedown', e => !e.composedPath().includes(host) && hide(), true);
})();
