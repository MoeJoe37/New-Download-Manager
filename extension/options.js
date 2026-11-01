// The extension's own options: the download panel on or off, and the sites it is hidden on (content.js).
const store = chrome.storage.local;
const panel = document.getElementById('panel');
const list = document.getElementById('sites');

async function render() {
  const { panel_off = false, panel_sites_off = [] } = await store.get(['panel_off', 'panel_sites_off']);
  panel.checked = !panel_off;
  list.replaceChildren();
  if (!panel_sites_off.length) {
    const li = document.createElement('li');
    li.className = 'empty';
    li.style.background = 'none';
    li.style.border = '0';
    li.textContent = 'None. Right-click the panel on a site and choose "Don\'t show on" to add it.';
    list.append(li);
  }
  for (const site of panel_sites_off) {
    const li = document.createElement('li');
    const name = document.createElement('span');
    name.textContent = site;
    const show = document.createElement('button');
    show.textContent = 'Show again';
    show.onclick = async () => store.set({ panel_sites_off: panel_sites_off.filter(s => s !== site) });
    li.append(name, show);
    list.append(li);
  }
}

panel.onchange = () => store.set({ panel_off: !panel.checked });
chrome.storage.onChanged.addListener(render);
render();
