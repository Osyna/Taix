// Two small pieces of page chrome: the "Reconnecting…" banner while the
// socket is down, and the shortcut sheet a hardware keyboard summons by
// holding Command (Meta).

import { h, mobile } from './store.js';

// ---------- connection banner ----------

let banner = null;
let tick = null;

/** The socket is down: say so, with how stale the screen is. */
export function offline(since) {
  banner ??= document.body.appendChild(h('div', { id: 'conn', class: 'toast warn', role: 'status' }));
  const paint = () => {
    banner.textContent = `Reconnecting… last update ${Math.round((Date.now() - since) / 1000)} s ago`;
  };
  paint();
  banner.hidden = false;
  clearInterval(tick);
  tick = setInterval(paint, 1000);
}

/** A snapshot arrived again. */
export function online() {
  clearInterval(tick);
  if (banner) banner.hidden = true;
}

// ---------- shortcut sheet ----------

// What `app.js` `shortcut()`, the palette and the composer actually bind.
const apple = /Mac|iPhone|iPad/.test(navigator.platform);
const mod = apple ? '⌘' : 'Ctrl+Shift+';

const KEYS = [
  [`${apple ? '⌘' : 'Ctrl+'}K`, 'Jump to a terminal, project or command'],
  [`${mod}1–9`, 'Show window 1–9 of this project'],
  [`${mod}[  ${mod}]`, 'Previous / next window'],
  [`${mod}↑  ${mod}↓`, 'Previous / next project'],
  [`${mod}T`, 'New terminal'],
  [`${mod}.`, 'Take control and send Ctrl-C'],
  [`${apple ? '⌘' : 'Ctrl'} + scroll`, 'Terminal font size'],
  ['Enter  ·  Shift+Enter', 'Send the line  ·  new line in the composer'],
];

let sheet = null;
let timer = null;

function hide() {
  clearTimeout(timer);
  timer = null;
  if (sheet) sheet.hidden = true;
}

function show() {
  timer = null;
  if (mobile()) return;
  sheet ??= document.body.appendChild(
    h(
      'div',
      { id: 'keys-help', role: 'dialog', 'aria-label': 'Keyboard shortcuts' },
      h('b', { text: 'Keyboard shortcuts' }),
      ...KEYS.map(([keys, what]) => h('div', {}, h('kbd', { text: keys }), h('span', { text: what }))),
    ),
  );
  sheet.hidden = false;
}

document.addEventListener('keydown', (e) => {
  if (e.key !== 'Meta') return hide();
  // Command held on its own: any other key turns it into a shortcut.
  if (!e.repeat && !e.ctrlKey && !e.altKey && !e.shiftKey && !timer) timer = setTimeout(show, 600);
});
document.addEventListener('keyup', (e) => {
  if (e.key === 'Meta') hide();
});
window.addEventListener('blur', hide);
