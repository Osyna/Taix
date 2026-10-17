// TaiX hosts this browser knows about. Each host keeps its own tmux server;
// switching just navigates, and pairing is per origin.
import * as store from './store.js';

const KEY = 'taix-hosts';

/** Saved hosts, with this origin always present (labelled from the snapshot). */
export function hosts() {
  let list = [];
  try {
    list = JSON.parse(localStorage.getItem(KEY) || '[]');
  } catch (e) {}
  list = list.filter((x) => x && typeof x.url === 'string' && typeof x.label === 'string');
  const here = list.find((x) => x.url === location.origin);
  const label = store.state.snap?.host;
  if (!here) {
    list.unshift({ label: label || location.host, url: location.origin });
    save(list);
  } else if (label && here.label === location.host) {
    here.label = label;
    save(list);
  }
  return list;
}

function save(list) {
  localStorage.setItem(KEY, JSON.stringify(list));
}

/** Add a host; returns an error string or null. */
export function addHost(label, url) {
  let u;
  try {
    u = new URL(url.includes('://') ? url : 'http://' + url);
  } catch (e) {
    return 'Not a valid URL';
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return 'Use http:// or https://';
  const list = hosts().filter((x) => x.url !== u.origin);
  list.push({ label: label.trim() || u.host, url: u.origin });
  save(list);
  return null;
}

export function removeHost(url) {
  save(hosts().filter((x) => x.url !== url || x.url === location.origin));
}

export function switchHost(url) {
  if (url !== location.origin) location.href = url + '/';
}

/** The Settings section. */
export function section() {
  const h = store.h;
  const box = h('div', { class: 'panel-section' });
  const draw = () => {
    const label = h('input', { type: 'text', placeholder: 'Label' });
    const url = h('input', { type: 'text', placeholder: 'https://box.tail1234.ts.net or http://192.168.1.5:4040' });
    const err = h('div', { class: 'row-sub' });
    const add = h('button', {
      text: 'Add',
      onclick: () => {
        const e = addHost(label.value, url.value);
        if (e) err.textContent = e;
        else draw();
      },
    });
    box.replaceChildren(
      h('div', { class: 'panel-label' }, 'Hosts'),
      ...hosts().map((x) =>
        h('div', { class: 'field' },
          h('button', { text: x.label + (x.url === location.origin ? ' (this)' : ''), onclick: () => switchHost(x.url) }),
          h('span', { class: 'mono', text: x.url }),
          x.url === location.origin ? '' : h('button', { text: 'Remove', onclick: () => { removeHost(x.url); draw(); } }),
        ),
      ),
      h('div', { class: 'field' }, label, url, add),
      err,
      h('div', { class: 'row-sub', text: 'Each host has its own tmux server. Pairing is per host: one you have not paired shows its own pairing page.' }),
    );
  };
  draw();
  return box;
}
