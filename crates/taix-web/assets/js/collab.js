// Sharing between people and devices: a read-only link for one window, a
// name for this browser, and sending a window to another device. The server
// half is `collab.rs`; the id every request carries is `device` in
// store.js, which is how the hub knows who is asking.

import { state, el, h, api, device, toast, failed, copy, menu, view, claim, windowById } from './store.js';

const q = `?d=${device}`;

/** The address a person elsewhere can reach: not "localhost" on the host. */
function linkFor(token) {
  const local = /^(localhost|127\.|\[::1\])/.test(location.hostname);
  const addr = state.snap?.health?.addr;
  const origin = local && addr ? `http://${addr}` : location.origin;
  return `${origin}/s/${token}/`;
}

const span = (secs) => (secs >= 3600 ? `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m` : `${Math.max(1, Math.ceil(secs / 60))}m`);

async function share(win) {
  try {
    const { token, secs } = await api(`/collab/share${q}`, { window: win.id });
    await copy(linkFor(token), `Read-only link copied · ends in ${span(secs)}`);
  } catch (e) {
    failed(e);
  }
}

async function sendTo(win) {
  try {
    const { devices } = await api(`/collab/devices${q}`);
    if (!devices.length) return toast('No other device is connected', 'warn');
    menu(
      `Send ${win.name} to…`,
      devices.map((d) => ({
        label: d.name,
        run: async () => {
          try {
            await api(`/collab/send${q}`, { to: d.id, window: win.id, name: win.name });
            toast(`Sent to ${d.name}`);
          } catch (e) {
            failed(e);
          }
        },
      })),
    );
  } catch (e) {
    failed(e);
  }
}

/** Items for a window's menu. */
export function windowItems(win) {
  return [
    { label: 'Share read-only', hint: 'Link, ends in 1 h', run: () => share(win) },
    { label: 'Send to…', hint: 'Another device', run: () => sendTo(win) },
  ];
}

/** Show it here and take the keyboard: what "Open" on a hand-off means. */
function openHere(id) {
  const win = windowById(id);
  if (!win) return toast('That window is gone', 'warn');
  state.shown = id;
  state.selected = win.project;
  state.zoomed = null;
  view('stage');
  claim(id);
}

/** Another device sent this page a window: a toast that outlasts the usual. */
export function notice({ from, window: id, name }) {
  const gone = () => {
    node.classList.add('out');
    setTimeout(() => node.remove(), 300);
  };
  const node = h(
    'div',
    { class: 'toast', style: 'display:flex;gap:12px;align-items:center;pointer-events:auto' },
    h('span', { text: `${name || 'A window'} sent from ${from}` }),
    h('button', {
      class: 'accent',
      text: 'Open',
      onclick: () => {
        gone();
        openHere(id);
      },
    }),
  );
  el('toasts').append(node);
  setTimeout(gone, 30000);
}

/** Settings: this device's name, and the links that are alive. */
export function settingsSection() {
  const name = h('input', { type: 'text', maxlength: '40', placeholder: 'Name this device' });
  name.onchange = async () => {
    try {
      const res = await api(`/collab/name${q}`, { name: name.value });
      name.value = res.name;
      toast('Device renamed');
    } catch (e) {
      failed(e);
    }
  };
  const links = h('div', {});

  const load = async () => {
    const { shares } = await api('/collab/shares');
    links.replaceChildren(
      shares.length
        ? null
        : h('div', { class: 'info-row' }, h('span', { class: 'k', text: 'none' })),
      ...shares.map((s) =>
        h(
          'div',
          { class: 'info-row' },
          h('span', { class: 'k', text: windowById(s.window)?.name ?? `window ${s.window}` }),
          h('span', { class: 'v mono', text: `ends in ${span(s.secs)}` }),
          h('button', { class: 'chip', text: 'Copy', onclick: () => copy(linkFor(s.token), 'Link copied') }),
          h('button', {
            class: 'chip',
            text: 'Revoke',
            onclick: async () => {
              try {
                await api('/collab/revoke', { token: s.token });
                toast('Link revoked');
                load();
              } catch (e) {
                failed(e);
              }
            },
          }),
        ),
      ),
    );
  };
  load().catch(() => {});
  api(`/collab/devices${q}`).then((res) => { name.value = res.me.name; }, () => {});

  return h(
    'div',
    { class: 'panel-section' },
    h('div', { class: 'panel-label' }, 'This device'),
    h('div', { class: 'field' }, h('label', { text: 'Name' }), name),
    h('div', { class: 'panel-label' }, 'Read-only links'),
    links,
  );
}
