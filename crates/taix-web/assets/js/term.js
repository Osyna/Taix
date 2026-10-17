// The terminal side of the page: the panes, the grid they are drawn at, and
// the pointer and hardware-keyboard paths into them. Everything a thumb
// types with lives in `compose.js`.

import {
  state,
  on,
  onStaged,
  onReconnect,
  emit,
  send,
  mobile,
  driving,
  claim,
  selected,
  shown,
  pane,
  windows,
  windowById,
  projects,
  project,
  tint,
  h,
  icon,
  sym,
  el,
  client,
  menu,
  ask,
  spawnHere,
  copy,
  api,
  toast,
  layout,
  bytes,
  ago,
  drop as release,
  tap,
  lag,
} from './store.js';
import { age } from './age.js';
import { focusInput, buzz, takeMods, liveTyping, attachSnippet } from './compose.js';
import { heldBy } from './store.js';
import { windowItems } from './collab.js';

// Client-side font scale, persisted locally. Never sent to the server.
let scale = parseFloat(localStorage.getItem('taix-font-scale') || '1.0');

// Measure a monospace cell once to set the base font size.
let cellWidth = 0;
let cellHeight = 0;

// Cache of pane cards by window id: { el, html, cols, rows }.
const paneCache = new Map();

// Composing flag to avoid double-send on hardware input methods.
let composing = false;

/** `tap` on a node: acts on the press for a mouse, on the click otherwise. */
function bindTap(node, fn, early) {
  const t = tap(fn, early);
  node.addEventListener('pointerdown', t.onpointerdown);
  node.addEventListener('click', t.onclick);
}

/** True while the page's own text selection sits inside `node`. */
function selecting(node) {
  const s = getSelection();
  return !s.isCollapsed && node.contains(s.anchorNode);
}

// Select mode: every drag selects text, even in a program that takes the
// mouse - Shift+drag without holding Shift. Toggled from the right-click menu.
let selectMode = false;

function setSelectMode(on) {
  selectMode = on;
  el('grid').classList.toggle('select-mode', on);
  toast(on ? 'Select mode: drag to select, Esc to leave' : 'Select mode off');
}

/** Paste the clipboard into window `id`. */
async function pasteInto(id) {
  if (!driving(id)) claim(id);
  let text;
  try {
    text = await navigator.clipboard.readText();
  } catch {
    // Plain http on a LAN is not a secure context, so the page may not read
    // the clipboard; the browser's own paste into the input still works.
    focusInput();
    return toast(`Press ${/Mac/.test(navigator.platform) ? 'Cmd+V' : 'Ctrl+V'} to paste`);
  }
  if (text) send({ kind: 'paste', window: id, text });
}

export function mountTerm() {
  const grid = el('grid');
  const sink = el('sink');

  measureCell();
  trackViewport();
  applyFont();

  let lastWatchList = [];
  // A page nobody is looking at asks for no screens at all.
  let away = document.visibilityState === 'hidden';

  // Watch: send the visible window set on change and every 10s, so the
  // desktop keeps capturing panes it is not itself looking at.
  function sendWatch(visible) {
    const ids = [...new Set([...visible, ...state.snap.lock.remote])].sort((a, b) => a - b);
    const key = ids.join(',');
    if (key !== lastWatchList.join(',')) {
      if (!away) send({ kind: 'watch', client, windows: ids });
      lastWatchList = ids;
    }
  }

  setInterval(() => {
    if (!away && lastWatchList.length > 0) {
      send({ kind: 'watch', client, windows: lastWatchList });
    }
  }, 10000);
  onReconnect(() => {
    if (!away && lastWatchList.length > 0) send({ kind: 'watch', client, windows: lastWatchList });
  });
  document.addEventListener('visibilitychange', () => {
    away = document.visibilityState === 'hidden';
    send({ kind: 'watch', client, windows: away ? [] : lastWatchList });
  });

  // Layout buttons.
  for (const btn of el('layout').querySelectorAll('[data-layout]')) {
    bindTap(btn, () => {
      const mode = btn.dataset.layout;
      if (mode) layout(mode);
    });
  }

  // A tap on a pane. On a laptop that is the claim gesture and the "type
  // here" gesture at once, the same as clicking a pane on the desktop.
  //
  // On a phone it is neither: the pane fills the screen, so a tap is how
  // you scroll and how you read, and claiming would take the keyboard off
  // the desktop and reflow the tmux window to a phone's grid for someone
  // who only wanted to look. There, typing is what claims - see
  // `compose.js` - so a tap only chooses which window is on screen.
  grid.addEventListener('click', (e) => {
    const paneEl = e.target.closest('.pane');
    if (!paneEl) return;
    const id = parseInt(paneEl.dataset.id, 10);
    if (state.shown !== id) {
      state.shown = id;
      emit();
    }
    // A link opens in a new tab - unless the program wants the click; then
    // Ctrl/Cmd reaches the link underneath, as on the desktop.
    const p = pane(id);
    if (!(p?.mouse && driving(id)) || e.ctrlKey || e.metaKey) {
      const url = !getSelection().toString() && linkAt(e, paneEl);
      if (url) return void window.open(url, '_blank', 'noopener');
    }
    // A drag that selected text is a copy, not "type here": focusing the
    // input would move the selection into it and drop what was picked.
    if (selecting(paneEl) || selectMode) return;
    // Live typing is the exception: there a tap is "type here", as on a laptop.
    if (mobile() && !liveTyping()) return;
    if (!driving(id)) claim(id);
    focusInput();
  });

  // The pane is chosen on the press, ahead of the click that claims it: a
  // drag that selects text must not have moved the focus first.
  grid.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || e.pointerType === 'touch' || e.target.closest('.pane-head')) return;
    const paneEl = e.target.closest('.pane');
    if (!paneEl) return;
    const id = parseInt(paneEl.dataset.id, 10);
    if (state.shown !== id) {
      state.shown = id;
      emit();
    }
  });

  // Right-click: the terminal's own menu. Shift+right-click keeps the
  // browser's; a phone has its long press instead.
  grid.addEventListener('contextmenu', (e) => {
    const paneEl = e.target.closest('.pane');
    if (!paneEl || e.shiftKey || mobile()) return;
    e.preventDefault();
    const id = parseInt(paneEl.dataset.id, 10);
    const card = paneCache.get(id);
    // Read now: clicking a menu item may drop the selection.
    const picked = selecting(paneEl) ? getSelection().toString() : '';
    const url = linkAt(e, paneEl);
    const at = { getBoundingClientRect: () => ({ left: e.clientX, bottom: e.clientY }) };
    menu('Terminal', [
      picked && { label: 'Copy', hint: 'Ctrl+C', run: () => copy(picked) },
      picked && { label: 'Attach selection', run: () => attachSnippet(picked) },
      { label: 'Paste', run: () => pasteInto(id) },
      url && { label: 'Open link', run: () => window.open(url, '_blank', 'noopener') },
      url && { label: 'Copy link', run: () => copy(url) },
      '-',
      { label: selectMode ? 'Leave select mode' : 'Select mode', hint: 'Shift+drag', run: () => setSelectMode(!selectMode) },
      card && { label: 'Select all', run: () => {
        sink.blur();
        getSelection().selectAllChildren(card.screen);
      }},
      card && { label: 'Copy screen', run: () => copy(screenLines(card).join('\n').trimEnd(), 'Copied screen') },
      '-',
      { label: 'More…', run: () => windowMenu(at) },
    ], at);
  });

  // The window strip and the header's window menu: mobile navigation.
  bindTap(
    el('wins'),
    (e) => {
      const tab = e.target.closest('.tab');
      if (!tab) return;
      if (tab.classList.contains('add')) {
        newWindow();
      } else if (tab.classList.contains('proj')) {
        projectMenu(tab);
      } else {
        const id = parseInt(tab.dataset.id, 10);
        if (id) {
          if (e.target.closest('.tab-x')) {
            send({ kind: 'close', window: id });
          } else if (state.shown !== id) {
            state.shown = id;
            emit();
          }
        }
      }
    },
    // Only choosing a window is safe to do on the press.
    (e) => !!e.target.closest('.tab[data-id]') && !e.target.closest('.tab-x'),
  );
  el('more').addEventListener('click', (e) => windowMenu(e.currentTarget));

  // The sink's own path: printable characters and an input method's output,
  // which `keydown` cannot spell. Every branch cancels the edit and leaves
  // the field empty, so it stays a keystroke pipe rather than a text box.
  sink.addEventListener('beforeinput', (e) => {
    if (composing) return;
    const id = shown();
    if (!id || !driving(id)) return;

    const type = e.inputType;
    let handled = true;

    if (type === 'insertText' && e.data) {
      const mods = takeMods();
      if (mods && e.data.length === 1) {
        predictKey(id, mods);
        send({ kind: 'key', window: id, name: mods + e.data });
      } else {
        predict(id, e.data);
        send({ kind: 'keys', window: id, text: e.data });
      }
    } else if (type === 'insertLineBreak' || type === 'insertParagraph') {
      predictKey(id, 'Enter');
      send({ kind: 'key', window: id, name: 'Enter' });
    } else if (type === 'deleteContentBackward') {
      predictKey(id, 'BSpace');
      send({ kind: 'key', window: id, name: 'BSpace' });
    } else if (type === 'deleteWordBackward') {
      predictKey(id, 'C-w');
      send({ kind: 'key', window: id, name: 'C-w' });
    } else if (type === 'deleteContentForward') {
      predictKey(id, 'DC');
      send({ kind: 'key', window: id, name: 'DC' });
    } else {
      handled = false;
    }

    if (handled) {
      e.preventDefault();
      sink.value = '';
    }
  });

  // A paste - Ctrl+V, Cmd+V, the browser's menu - goes to the program as
  // typing, or as one bracketed paste when it has several lines. Read from
  // the event itself: it carries the text even on plain http, where the
  // page may not read the clipboard on its own.
  // On the document, so it lands while a selection has focus off the sink.
  document.addEventListener('paste', (e) => {
    if (e.target !== sink && e.target.closest?.('input, textarea, [contenteditable]')) return;
    const id = shown();
    if (!id || !driving(id)) return;
    const text = e.clipboardData?.getData('text/plain') || '';
    e.preventDefault();
    predictKey(id, 'paste');
    if (text) send({ kind: text.includes('\n') ? 'paste' : 'type', window: id, text });
  });

  sink.addEventListener('compositionstart', () => {
    composing = true;
  });

  sink.addEventListener('compositionend', (e) => {
    composing = false;
    const id = shown();
    if (id && driving(id) && e.data) {
      predictKey(id, 'compose');
      send({ kind: 'keys', window: id, text: e.data });
      sink.value = '';
    }
  });

  // Named keys from a hardware keyboard. One listener, on the document: the
  // sink is inside it, and two listeners saw the same bubbling event twice,
  // which sent every Backspace and Enter twice.
  document.addEventListener('keydown', (e) => {
    // A real text field - the composer, the rename dialog, the filter box -
    // keeps its own keys. The sink is not one of those: it *is* the
    // terminal's input.
    if (e.target !== sink && (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA')) {
      return;
    }
    if (selectMode && e.key === 'Escape') {
      e.preventDefault();
      return setSelectMode(false);
    }
    const id = shown();
    if (!id || !driving(id)) return;
    // Ctrl/Cmd+C over text picked in a pane copies it, as in any terminal
    // with a selection; ^C goes to the program only when nothing is picked.
    // Ctrl+V (and Ctrl+Shift+V) is the browser's paste, as in a web
    // terminal: a raw ^V never let the clipboard in. A Mac pastes with Cmd.
    const lower = e.key.toLowerCase();
    if ((e.ctrlKey || e.metaKey) && lower === 'c' && selecting(grid)) return;
    if (e.ctrlKey && lower === 'v' && !/Mac/.test(navigator.platform)) return;
    const cmd = translateKey(e);
    if (cmd) {
      // Cancels the key's own default action too, so no `beforeinput`
      // follows it and the same keystroke is not sent a second time.
      e.preventDefault();
      predictKey(id, cmd.name);
      send({ ...cmd, window: id });
    } else if (e.target !== sink && !e.ctrlKey && !e.metaKey && e.key.length === 1) {
      // A selection took focus off the input; the next character is still
      // typing, so it goes back there before the browser delivers it.
      sink.focus();
    }
  });

  grid.addEventListener('wheel', (e) => {
    const paneEl = e.target.closest('.pane');
    if (!paneEl) return;
    const id = parseInt(paneEl.dataset.id, 10);
    const p = pane(id);
    if (!p) return;

    e.preventDefault();
    // Ctrl/Cmd with a wheel is zoom everywhere else, so it is zoom here -
    // of the terminal, which is the thing under the pointer. Cancelling it
    // is what keeps the browser from scaling the whole page instead.
    if (e.ctrlKey || e.metaKey) {
      setScale(scale * (e.deltaY > 0 ? 0.95 : 1.05));
      return;
    }
    if (p.mouse && driving(id)) {
      const button = e.deltaY > 0 ? 65 : 64;
      const { col, row } = eventToCell(e, paneEl);
      if (col !== null) {
        send({ kind: 'mouse', window: id, button, col, row, motion: 'press' });
      }
    } else {
      // A positive delta is a step back into tmux's history, so a wheel
      // turned down - towards newer output - is negative.
      queueScroll(id, e.deltaY > 0 ? -20 : 20);
    }
  }, { passive: false });

  grid.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    const body = e.target.closest('.pane-body');
    if (!body || !body.parentElement.classList.contains('pane')) return;
    const id = parseInt(body.parentElement.dataset.id, 10);
    const p = pane(id);
    // Shift reaches the page's own selection, as in xterm and every
    // terminal since: a TUI that takes the mouse otherwise owns every drag.
    // Select mode is the same without holding Shift.
    // With the input focused, Shift+press would only extend the caret
    // inside it, so the drag selects nothing; it lets go of the input first.
    const free = e.shiftKey || selectMode;
    if (free && document.activeElement === sink) {
      sink.blur();
      getSelection().removeAllRanges();
    }
    if (!p || !p.mouse || !driving(id) || free) return;

    e.preventDefault();
    const { col, row } = eventToCell(e, body.parentElement);
    if (col !== null) {
      send({ kind: 'mouse', window: id, button: 0, col, row, motion: 'press' });
      const onUp = () => {
        send({ kind: 'mouse', window: id, button: 0, col, row, motion: 'release' });
        document.removeEventListener('mouseup', onUp);
      };
      document.addEventListener('mouseup', onUp);
    }
  });

  touchGestures(grid);

  // Pinch sets the font size, and only the font size: the page around the
  // terminal keeps its own layout, so zooming in does not push the key row
  // or the composer off the screen. That means taking the gesture from the
  // browser - `touch-action` on the grid removes its pinch-zoom, these
  // handlers cancel what is left, and Safari's `gesture*` events are
  // cancelled too because iOS zooms from those rather than from touches.
  //
  // The grid follows the font, not the other way around: fitting a
  // 137-column pane into a phone's width produced 4px text nobody could
  // read.
  let pinch = null;
  grid.addEventListener('touchstart', (e) => {
    if (e.touches.length !== 2) return;
    const [a, b] = e.touches;
    pinch = Math.hypot(b.clientX - a.clientX, b.clientY - a.clientY);
    e.preventDefault();
  }, { passive: false });

  grid.addEventListener('touchmove', (e) => {
    if (e.touches.length !== 2 || !pinch) return;
    e.preventDefault();
    const [a, b] = e.touches;
    const dist = Math.hypot(b.clientX - a.clientX, b.clientY - a.clientY);
    setScale(scale * (dist / pinch));
    pinch = dist;
  }, { passive: false });

  grid.addEventListener('touchend', (e) => {
    if (e.touches.length < 2) pinch = null;
  }, { passive: true });

  for (const name of ['gesturestart', 'gesturechange', 'gestureend']) {
    grid.addEventListener(name, (e) => e.preventDefault());
  }

  // The staged line is echoed at the prompt, so what you are typing looks
  // like it is in the terminal even though it has not been sent.
  onStaged(paintGhost);

  on((snap) => {
    const visible = renderPanes(snap);
    renderWins(snap);
    renderStageHead(snap);
    reportGrid();
    sendWatch(visible);
  });
}

// ---------- scroll ----------

// Pending scroll requests, coalesced into one command per window per frame.
const scrolls = new Map();
let scrollFrame = null;

function queueScroll(id, delta) {
  scrolls.set(id, (scrolls.get(id) || 0) + delta);
  if (scrollFrame) return;
  scrollFrame = requestAnimationFrame(() => {
    scrollFrame = null;
    for (const [window, amount] of scrolls) {
      if (amount) send({ kind: 'scroll', window, delta: amount });
    }
    scrolls.clear();
  });
}

// ---------- touch ----------

// One finger on a pane is one of four things, decided by what it does next:
//
//   pending  down, undecided. Held still for LONG_PRESS it becomes `sel`;
//            moved LOCK px it becomes `h` or `v` by the larger axis.
//   v        a vertical drag: scroll. tmux owns the scrollback, so for a
//            plain shell it is a `scroll` request; for a program that took
//            the mouse it is wheel events, like the desktop wheel. Lifting
//            at speed coasts on.
//   h        a horizontal drag on a phone: swipe to the next or previous
//            window in the strip. The pane follows the finger and snaps
//            back unless the swipe is long or quick enough.
//   sel      whole rows picked for copying; a drag extends them, and the
//            bar stays after the finger lifts.
//
// A second finger abandons all of it: that is the pinch's.
const LONG_PRESS = 450;
const SLOP = 8; // further than this and a press is not a long press
const LOCK = 10; // how far a drag goes before its axis is decided
const EDGE = 20; // iOS's back gesture lives here, so no swipe starts in it
const SWIPE = 60; // px, or a flick of FLICK px/ms over at least 20
const FLICK = 0.5;
const WHEELS = 12; // wheel events per frame; a flung thumb must not flood tmux

let touch = null; // the finger that is down
let sel = null; // { id, card, a, b, band, bar }: `a` is the anchor, `b` the end that moves
let coast = 0; // the momentum frame, if one is running

const reducedMotion = () => matchMedia('(prefers-reduced-motion: reduce)').matches;

// Wheel events for a mouse-mode program, coalesced per frame like `scrolls`.
const wheels = new Map();
let wheelFrame = null;

function queueWheel(id, rows, at) {
  const w = wheels.get(id) || { rows: 0 };
  w.rows += rows;
  w.at = at;
  wheels.set(id, w);
  if (wheelFrame) return;
  wheelFrame = requestAnimationFrame(() => {
    wheelFrame = null;
    for (const [window, { rows, at }] of wheels) {
      // Content follows the finger: dragging down is wheel up.
      const button = rows > 0 ? 64 : 65;
      for (let i = Math.min(Math.abs(rows), WHEELS); i > 0; i--) {
        send({ kind: 'mouse', window, button, ...at, motion: 'press' });
      }
    }
    wheels.clear();
  });
}

/** Scroll by whole rows, positive towards older output, whichever way the pane wants it. */
function scrollRows(id, rows, paneEl, point) {
  const p = pane(id);
  if (!p) return;
  if (p.mouse && driving(id)) {
    let at = eventToCell(point, paneEl);
    if (at.col === null) at = { col: p.cols >> 1, row: p.rows >> 1 };
    queueWheel(id, rows, at);
  } else {
    queueScroll(id, rows);
  }
}

/** Decaying scroll after a flick: `v` is rows per ms, in the drag's own sign. */
function startCoast(id, paneEl, point, v) {
  let last = performance.now();
  let owed = 0;
  const step = (now) => {
    const dt = now - last;
    last = now;
    v *= 0.94 ** (dt / 16);
    owed += v * dt;
    const rows = Math.trunc(owed);
    owed -= rows;
    if (rows) scrollRows(id, rows, paneEl, point);
    const p = pane(id);
    // Nothing newer than the live bottom to coast towards.
    const spent = !p || (rows < 0 && !p.mouse && p.scroll === 0);
    coast = Math.abs(v) > 0.004 && !spent && paneEl.isConnected ? requestAnimationFrame(step) : 0;
  };
  coast = requestAnimationFrame(step);
}

function stopCoast() {
  cancelAnimationFrame(coast);
  coast = 0;
}

/** The window `dir` steps along the strip from the one on screen, if it can be shown. */
function neighbour(dir) {
  const here = shown();
  const proj = state.snap?.projects.find((p) => p.id === (windowById(here)?.project ?? selected()));
  const strip = (proj?.windows || []).filter((w) => w.alive || w.kind === 'browser');
  const at = strip.findIndex((w) => w.id === here);
  return at < 0 ? null : strip[at + dir]?.id ?? null;
}

/** Show a window as the strip's tab does, then bring it in from the side it came from. */
function swipeTo(id, dir) {
  state.shown = id;
  emit();
  if (reducedMotion()) return;
  paneCache.get(id)?.body.animate(
    [{ transform: `translateX(${dir * 40}px)`, opacity: 0.3 }, { transform: 'none', opacity: 1 }],
    { duration: 160, easing: 'ease-out' },
  );
}

function snapBack(card) {
  const style = card.body.style;
  style.transition = 'transform 160ms ease-out';
  style.transform = '';
  setTimeout(() => { style.transition = ''; }, 180);
}

// ---- selection ----

/** The screen's rows as text, without the staged line the ghost echoes. */
function screenLines(card) {
  const copyOf = card.screen.cloneNode(true);
  copyOf.querySelector('.ghost')?.remove();
  copyOf.querySelector('.predict')?.remove();
  return copyOf.textContent.split('\n').map((line) => line.trimEnd());
}

/** The http(s) URL under a pointer event on `paneEl`, or null. */
function linkAt(e, paneEl) {
  const { col, row } = eventToCell(e, paneEl);
  if (col === null) return null;
  const line = paneEl.querySelector('.screen').textContent.split('\n')[row] || '';
  for (const m of line.matchAll(/https?:\/\/[^\s"'<>`]+/g)) {
    const url = m[0].replace(/[.,;:!?)\]}]+$/, '');
    if (col >= m.index && col < m.index + url.length && url.length > 8) return url;
  }
  return null;
}

function rowAt(card, y) {
  const last = card.screen.textContent.split('\n').length - 1;
  const row = Math.floor((y - card.screen.getBoundingClientRect().top) / cell().h);
  return Math.max(0, Math.min(last, row));
}

function selectedText() {
  const lo = Math.min(sel.a, sel.b);
  const hi = Math.max(sel.a, sel.b);
  return screenLines(sel.card).slice(lo, hi + 1).join('\n');
}

/** The line goes into the composer as typing would, and stays there unsent. */
function pasteIntoLine(text) {
  const field = el('entry-text');
  const line = text.replace(/\s+/g, ' ').trim();
  if (!line) return;
  const pad = field.value && !/\s$/.test(field.value) ? ' ' : '';
  field.value += pad + line;
  field.dispatchEvent(new Event('input'));
  field.focus();
}

function paintSel() {
  const { card, a, b, band, bar } = sel;
  const lo = Math.min(a, b);
  const hi = Math.max(a, b);
  const { h: rowH } = cell();
  const s = card.screen;
  Object.assign(band.style, {
    top: `${s.offsetTop + lo * rowH}px`,
    left: `${s.offsetLeft}px`,
    width: `${s.offsetWidth}px`,
    height: `${(hi - lo + 1) * rowH}px`,
  });
  // Just above the picked rows, or just below when they touch the top;
  // kept inside the visible output either way.
  const box = card.el.getBoundingClientRect();
  const view = card.body.getBoundingClientRect();
  const rows = band.getBoundingClientRect();
  const gap = 8;
  const barH = bar.offsetHeight;
  let top = rows.top - barH - gap < view.top ? rows.bottom + gap : rows.top - barH - gap;
  top = Math.max(view.top + gap, Math.min(top, view.bottom - barH - gap));
  bar.style.top = `${top - box.top}px`;
}

function startSel(card, id, row) {
  endSel();
  const done = (fn) => () => {
    fn();
    endSel();
  };
  const band = h('div', { class: 'sel-band' });
  const bar = h(
    'div',
    { class: 'sel-bar' },
    h('button', { type: 'button', onclick: done(() => copy(selectedText())) }, 'Copy'),
    h('button', { type: 'button', onclick: done(() => copy(screenLines(card).join('\n').trimEnd())) }, 'Copy screen'),
    h('button', { type: 'button', onclick: done(() => pasteIntoLine(selectedText())) }, 'Paste into line'),
    h('button', { type: 'button', onclick: done(() => attachSnippet(selectedText())) }, 'Attach'),
    h('button', { type: 'button', class: 'sel-close', title: 'Close', 'aria-label': 'Close', onclick: endSel, html: '<svg class="ico"><use href="#i-close" /></svg>' }),
  );
  card.body.append(band);
  card.el.append(bar);
  card.hold = true;
  card.body.addEventListener('scroll', paintSel, { passive: true });
  sel = { id, card, a: row, b: row, band, bar };
  paintSel();
}

function endSel() {
  if (!sel) return;
  sel.band.remove();
  sel.card.body.removeEventListener('scroll', paintSel);
  sel.bar.remove();
  sel.card.hold = false;
  sel = null;
}

function touchGestures(grid) {
  // Not a gesture the browser may keep: it would scroll the box we are
  // already scrolling, or select the line we are about to.
  const swallow = (e) => e.cancelable && e.preventDefault();

  const drop = () => {
    if (!touch) return;
    clearTimeout(touch.timer);
    if (touch.mode === 'h') snapBack(touch.card);
    touch = null;
  };

  grid.addEventListener('touchstart', (e) => {
    stopCoast();
    if (e.touches.length !== 1) return drop();
    const paneEl = e.target.closest('.pane');
    if (!paneEl || e.target.closest('button')) return;
    // A finger on the output while the composer has the keyboard is a
    // scroll, not a "stop typing": left to itself iOS would blur the field
    // and drop the keyboard, which is the single most annoying thing a
    // terminal on a phone can do. The pane's own buttons keep their tap.
    // Cancelling touchstart also cancels iOS's own long-press callout, so
    // the long press below is a timer, not that.
    if (document.activeElement === el('entry-text')) e.preventDefault();

    const id = parseInt(paneEl.dataset.id, 10);
    const card = paneCache.get(id);
    if (!card) return;
    if (sel && sel.card !== card) endSel();

    const { clientX: x, clientY: y } = e.touches[0];
    touch = {
      id, paneEl, card, x0: x, y0: y, y, ly: y, lx: x, lt: performance.now(),
      v: 0, vx: 0, point: { clientX: x, clientY: y }, mode: 'pending', edge: x < EDGE,
    };

    if (sel) {
      // The end nearest the finger is the one that moves.
      const row = rowAt(card, y);
      if (Math.abs(row - sel.a) < Math.abs(row - sel.b)) [sel.a, sel.b] = [sel.b, sel.a];
      sel.b = row;
      paintSel();
      touch.mode = 'sel';
      return;
    }
    touch.timer = setTimeout(() => {
      if (touch?.mode !== 'pending') return;
      touch.mode = 'sel';
      buzz(15);
      startSel(card, id, rowAt(card, touch.y0));
    }, LONG_PRESS);
  }, { passive: false });

  grid.addEventListener('touchmove', (e) => {
    if (!touch) return;
    if (e.touches.length !== 1) return drop();
    swallow(e);
    const { clientX: x, clientY: y } = e.touches[0];
    const now = performance.now();
    const dx = x - touch.x0;
    const dy = y - touch.y0;
    touch.point = { clientX: x, clientY: y };

    if (touch.mode === 'pending') {
      const far = Math.hypot(dx, dy);
      if (far > SLOP) clearTimeout(touch.timer);
      if (far < LOCK) return;
      const swipe = Math.abs(dx) > Math.abs(dy) && mobile() && state.view === 'stage' && !touch.edge
        && (neighbour(1) || neighbour(-1));
      touch.mode = swipe ? 'h' : 'v';
    }

    if (touch.mode === 'sel') {
      const row = rowAt(touch.card, y);
      if (row !== sel.b) {
        sel.b = row;
        paintSel();
      }
      return;
    }

    const dt = now - touch.lt;
    if (dt > 0) {
      touch.v = touch.v * 0.6 + ((y - touch.ly) / dt) * 0.4;
      touch.vx = touch.vx * 0.6 + ((x - touch.lx) / dt) * 0.4;
    }
    touch.lt = now;
    touch.ly = y;
    touch.lx = x;

    if (touch.mode === 'h') {
      // Resisted where there is no window to go to.
      if (!reducedMotion()) touch.card.body.style.transform = `translateX(${neighbour(dx < 0 ? 1 : -1) ? dx : dx / 4}px)`;
      return;
    }
    // The content follows the finger: dragging down reveals what was above,
    // which is a step back into the history.
    const rows = Math.trunc((y - touch.y) / cell().h);
    if (!rows) return;
    touch.y += rows * cell().h;
    scrollRows(touch.id, rows, touch.paneEl, touch.point);
  }, { passive: false });

  grid.addEventListener('touchend', () => {
    if (!touch) return;
    const g = touch;
    clearTimeout(g.timer);
    touch = null;
    // A finger that rested before lifting is not a flick.
    const flung = performance.now() - g.lt < 100;
    if (g.mode === 'v') {
      if (flung && Math.abs(g.v) > 0.3) startCoast(g.id, g.paneEl, g.point, g.v / cell().h);
    } else if (g.mode === 'h') {
      const dx = g.point.clientX - g.x0;
      const far = Math.abs(dx) > SWIPE || (flung && Math.abs(g.vx) > FLICK && Math.abs(dx) > 20);
      const next = far ? neighbour(dx < 0 ? 1 : -1) : null;
      if (next) swipeTo(next, dx < 0 ? 1 : -1);
      else snapBack(g.card);
    }
  }, { passive: true });

  grid.addEventListener('touchcancel', drop, { passive: true });
}

// ---------- geometry ----------

/**
 * Keep the page the size of what the phone can actually see.
 *
 * iOS raises the keyboard over the page instead of resizing it: `100%` and
 * `100dvh` still mean the full screen, so the key row and the prompt sat
 * underneath the keyboard. Only `visualViewport` knows the truth, so the
 * height comes from there and the layout follows it. `--kb` is how much the
 * keyboard takes, which also tells the chrome to get out of the way.
 */
function trackViewport() {
  const vv = window.visualViewport;
  if (!vv) return;
  const doc = document.documentElement;
  const apply = () => {
    const kb = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop));
    doc.style.setProperty('--vh', `${Math.round(vv.height)}px`);
    // Where the visible part starts. iOS sometimes refuses `scrollTo(0, 0)`
    // while a field is focused, so the body is pinned to this offset
    // instead of hoping the layout viewport stays put.
    doc.style.setProperty('--vv-top', `${Math.round(vv.offsetTop)}px`);
    doc.style.setProperty('--kb', `${kb}px`);
    // A keyboard is tall; a URL bar shrinking by 60px is not.
    doc.classList.toggle('kb', kb > 120);
    // iOS 26, installed: the page is short of the screen by the status bar
    // and nothing is painted below it (see `html.short` in the stylesheet).
    // Only for a full-screen app: an iPad window in Split View or Stage
    // Manager is shorter than the screen by design.
    const standalone = navigator.standalone === true || matchMedia('(display-mode: standalone)').matches;
    const portrait = window.innerWidth < window.innerHeight;
    const tall = portrait ? Math.max(screen.width, screen.height) : Math.min(screen.width, screen.height);
    const wide = portrait ? Math.min(screen.width, screen.height) : Math.max(screen.width, screen.height);
    doc.classList.toggle('short', standalone && window.innerWidth >= wide - 1 && tall - window.innerHeight > 20);
    if (window.scrollY !== 0) window.scrollTo(0, 0);
    reportGrid();
  };
  vv.addEventListener('resize', apply);
  vv.addEventListener('scroll', apply);
  // The keyboard animates for ~300ms after focus changes, and Safari
  // reports its final geometry late or not at all; measure again once it
  // has settled.
  for (const type of ['focusin', 'focusout']) {
    document.addEventListener(type, () => {
      setTimeout(apply, 350);
      setTimeout(apply, 700);
    });
  }
  apply();
}

// The base size every pane is drawn at before the user's own zoom. Big
// enough to read on a phone held at arm's length; the grid is then cut to
// fit, rather than the text being shrunk until it fits the grid.
const BASE_FONT = 13;

// Measured at 16px with the line height `.screen` actually uses, so scaling
// by `size / 16` gives the real cell at any size.
function measureCell() {
  const probe = h('pre', { style: 'position:absolute;left:-9999px;top:0;font-family:var(--code);font-size:16px;line-height:1.3;margin:0;padding:0;white-space:pre;' }, 'M'.repeat(100));
  document.body.appendChild(probe);
  cellWidth = probe.offsetWidth / 100;
  cellHeight = probe.offsetHeight;
  probe.remove();
}

/** The pixel size of one cell as the screen is currently drawn. */
function cell() {
  const size = BASE_FONT * scale;
  return { w: (cellWidth * size) / 16, h: (cellHeight * size) / 16, size };
}

let persistTimer = null;

function setScale(next) {
  const clamped = Math.max(0.5, Math.min(3.0, next));
  if (clamped === scale) return;
  scale = clamped;
  applyFont();
  // A pinch produces a hundred of these; the disk hears about the last one.
  clearTimeout(persistTimer);
  persistTimer = setTimeout(() => localStorage.setItem('taix-font-scale', String(scale)), 400);
}

// The settings panel writes the same key and fires this; no round trip.
window.addEventListener('taix-font-scale', () => setScale(parseFloat(localStorage.getItem('taix-font-scale') || '1.0')));

let painted = 0;

/** Draw at the current size. Called from the pinch, not from every frame. */
function applyFont() {
  const { size } = cell();
  if (size === painted) return;
  painted = size;
  document.documentElement.style.setProperty('--term-font-size', `${size}px`);
  reportGrid();
}

// The grids last asked for, per window, and the frame that asks. A resize
// reflows every line of scrollback in tmux, so it waits for the rotation or
// the pinch to finish.
const sentGrid = new Map();
const dueGrid = new Map();
let gridTimer = null;
let gridSince = null;
let gridRetry = null;
let gridFrame = null;

// A phone held sideways shows two panes; each is sized from its own box.
const landscape = () => mobile() && window.matchMedia('(orientation: landscape) and (max-height: 500px)').matches;

// The window shown before this one, for the second pane.
let lastShown = null;
let prevShown = null;
// The partner pane last claimed, so it is claimed once per pairing.
let lastMate = null;
// The partner this page claimed only for the pair, given back when it ends.
let mateClaimed = null;

/** The other pane of a landscape pair: the previous window of this project, else the next one. */
function partner(win) {
  const mates = windows().filter((w) => w.project === win.project && w.id !== win.id && (w.alive || w.kind === 'browser'));
  return mates.find((w) => w.id === prevShown) || mates.find((w) => w.id > win.id) || mates[0] || null;
}

/**
 * Tell the desktop how big this page's terminal is.
 *
 * Only for a window this page drives: a second pair of eyes on a pane must
 * not reflow it under the person typing. The desktop resizes the tmux
 * window to this, and takes its widget's size back when the claim ends.
 *
 * Measured in a frame callback, because the caller is often mid-layout -
 * reading `clientWidth` there is a forced reflow on every snapshot.
 */
function reportGrid() {
  if (gridFrame) return;
  gridFrame = requestAnimationFrame(() => {
    gridFrame = null;
    // Both panes of a landscape pair, otherwise just the one being typed in.
    const ids = landscape() ? [...paneCache.keys()] : [shown()];
    for (const id of ids) {
      if (!id || !driving(id)) continue;
      const card = paneCache.get(id);
      const body = card?.body;
      if (!body || !body.clientWidth) continue;

      const pad = card.pad || (card.pad = padding(body));
      const width = body.clientWidth - pad.x;
      const height = body.clientHeight - pad.y;
      const { w, h } = cell();
      const cols = Math.max(20, Math.floor(width / w));
      const rows = Math.max(4, Math.floor(height / h));

      // Compared with what the snapshot says the pane *is*, not only with
      // what was sent: a request the desktop dropped (it arrived a beat
      // before the claim did) is asked again, once a second, until it lands.
      const p = pane(id);
      if (p && p.cols === cols && p.rows === rows) continue;
      const key = `${cols}x${rows}`;
      const last = sentGrid.get(id);
      const now = performance.now();
      if (last && last.key === key && now - last.at < 1000) {
        // An idle pane sends no snapshots to call this again.
        clearTimeout(gridRetry);
        gridRetry = setTimeout(reportGrid, 1000);
        continue;
      }
      sentGrid.set(id, { key, at: now });
      dueGrid.set(id, { cols, rows });
    }
    // Trailing, so a pinch sends only where it settles, but never later than
    // 600ms after the first change: a pane streaming output renders faster
    // than 150ms and would otherwise push the send back forever.
    if (!dueGrid.size) return;
    const now = performance.now();
    gridSince ??= now;
    clearTimeout(gridTimer);
    gridTimer = setTimeout(
      () => {
        gridTimer = gridSince = null;
        for (const [window, g] of dueGrid) send({ kind: 'grid', window, ...g });
        dueGrid.clear();
        // Look again once the desktop has had time to resize.
        clearTimeout(gridRetry);
        gridRetry = setTimeout(reportGrid, 1100);
      },
      now - gridSince > 600 ? 0 : 150,
    );
  });
}

/**
 * Put the end of the output in view when the grid is taller than its box.
 *
 * The grid is the desktop's until this page drives it, and `capture-pane`
 * pads it with blank rows: scrolling to the very bottom showed nothing but
 * the padding - a fresh shell's first line clipped away, a split's two
 * quiet panes blank. So the bottom aligns with the last row that printed
 * something; a full-screen program still lands on its last row.
 */
function followBottom(card) {
  const body = card.body;
  const room = body.scrollHeight - body.clientHeight;
  if (room <= 0) return;
  const used = card.screen.textContent.trimEnd().split('\n').length;
  const top = parseFloat(getComputedStyle(body).paddingTop);
  const end = top + (used * card.screen.offsetHeight) / card.rows;
  body.scrollTop = Math.min(room, Math.max(0, Math.ceil(end - body.clientHeight)));
}

function padding(node) {
  const style = getComputedStyle(node);
  return {
    x: parseFloat(style.paddingLeft) + parseFloat(style.paddingRight),
    y: parseFloat(style.paddingTop) + parseFloat(style.paddingBottom),
  };
}

// ---------- panes ----------

/**
 * Draw the panes this page is looking at, and return their ids.
 *
 * The card is built once per window and updated in place afterwards:
 * rebuilding it every frame threw away the text selection and the
 * keyboard's anchor, which on a phone is the difference between typing and
 * fighting the page.
 */
function renderPanes(snap) {
  const grid = el('grid');
  const empty = el('stage-empty');
  const isMobile = mobile();
  const shownId = shown();
  if (shownId !== lastShown) {
    prevShown = lastShown;
    lastShown = shownId;
  }

  let visible = [];
  if (isMobile && state.view === 'home') {
    // Home is the list, not the terminal: nothing is drawn, so the desktop
    // stops capturing for this page too.
    lastMate = null;
  } else if (isMobile) {
    // One pane, and it may live in a project this page is not listing:
    // tapping a window in the strip is how you get there.
    const win = windowById(shownId);
    if (win && (win.alive || win.kind === 'browser')) visible = [win];
    // A phone on its side has the width for two: the window it was just
    // looking at, kept in strip order so a tap does not swap the sides.
    const mate = visible.length && landscape() && partner(win);
    if (mate) {
      const order = windows().map((w) => w.id);
      visible = [win, mate].sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
      // Driven, so it is drawn at its half of the screen rather than at the
      // desktop's size; only when it joins, so a desktop that takes it back
      // keeps it.
      if (mate.id !== lastMate && mate.alive && !driving(mate.id)) {
        claim(mate.id);
        mateClaimed = mate.id;
      }
    }
    lastMate = mate ? mate.id : null;
  } else {
    const proj = snap.projects.find((p) => p.id === selected());
    const all = proj?.windows.filter((w) => w.alive) || [];
    if (state.zoomed) {
      visible = all.filter((w) => w.id === state.zoomed);
    } else if (state.layout === 'focus') {
      const win = all.find((w) => w.id === shownId);
      visible = win ? [win] : [];
    } else if (state.layout === 'split') {
      const idx = all.findIndex((w) => w.id === shownId);
      if (idx >= 0) {
        visible = [all[idx]];
        const next = all.slice(idx + 1).concat(all.slice(0, idx)).find((w) => w.alive);
        if (next) visible.push(next);
      }
    } else {
      // grid mode
      visible = all;
    }
  }
  // Rotated back, gone home or paired with another: the old partner would
  // otherwise stay squeezed to half a phone on the desktop.
  if (mateClaimed && mateClaimed !== lastMate && mateClaimed !== shownId) {
    if (state.held.has(mateClaimed)) release(mateClaimed);
    mateClaimed = null;
  }

  empty.hidden = visible.length > 0;
  grid.hidden = visible.length === 0;
  if (!visible.length) {
    // Every way onto the stage - a tab, the palette, a notification - can
    // land on a stopped window: say so, and make the button start it.
    const win = windowById(shownId);
    const stopped = win && !win.alive && win.kind !== 'browser';
    empty.querySelector('p').textContent = stopped ? `${win.name} is stopped.` : 'No window here yet.';
    empty.querySelector('button').textContent = stopped ? `Start ${win.name}` : 'Open a terminal';
  }
  grid.dataset.layout = state.layout || 'grid';
  grid.toggleAttribute('data-split', isMobile && visible.length > 1);

  const wids = visible.map((w) => w.id).join(',');
  if (grid.dataset.windows !== wids) {
    grid.dataset.windows = wids;
    endSel();
    paneCache.clear();
    grid.replaceChildren(
      ...visible.map((win) => {
        const p = snap.panes.find((pn) => pn.id === win.id);
        const card = buildPaneCard(win, p);
        paneCache.set(win.id, card);
        return card.el;
      }),
    );
  }

  for (const win of visible) {
    const card = paneCache.get(win.id);
    const p = snap.panes.find((pn) => pn.id === win.id);
    if (!card || !p) continue;

    // A selection holds its pane still: rows that move under the finger
    // are rows nobody can pick.
    const moved = card.lines !== p.lines && !card.hold && !selecting(card.screen) && paint(card, p.lines, p.shift);
    if (moved) {
      reconcile(card, p);
      paintGhost();
    }
    card.cols = p.cols;
    card.rows = p.rows;

    if (card.name !== win.name) {
      card.name = win.name;
      card.nameNode.textContent = win.name;
    }

    const harness = snap.harnesses?.find((entry) => entry.id === win.kind);
    const meta = [harness?.label || win.kind, win.branch, bytes(win.mem_kib)].filter(Boolean).join(' · ');
    if (card.meta !== meta) {
      card.meta = meta;
      card.metaNode.textContent = meta;
    }

    const stateClass = win.state || 'idle';
    if (card.stateClass !== stateClass) {
      if (card.stateClass) card.dot.classList.remove(card.stateClass);
      card.dot.classList.add(stateClass);
      card.stateClass = stateClass;
    }

    // Scrolled back: how far, and one tap to the live bottom.
    if (p.scroll > 0) {
      card.pill.hidden = false;
      card.pill.textContent = `↓ ${p.scroll}`;
    } else {
      card.pill.hidden = true;
    }

    // Who is typing here.
    const mine = driving(win.id);
    const other = heldBy(win.id);
    card.takeBtn.textContent = mine ? 'Reading' : other ? `Driven by ${other}` : 'Take control';
    card.el.classList.toggle('mine', mine);
    card.el.classList.toggle('cur', win.id === shownId);
    card.el.classList.toggle('zoomed', state.zoomed === win.id);

    // A fresh claim has to state the grid again even when this page's own
    // size never changed: the desktop's widget owned it in between.
    if (card.mine !== mine) {
      card.mine = mine;
      sentGrid.delete(win.id);
    }

    // A grid one row taller than the box hides the last line, which is
    // the only one that matters. Until the resize lands, follow the
    // bottom - while reading as much as while typing, or "back to live"
    // lands on a view the finger left halfway up. Only when something
    // moved: it is a layout read, and most frames move nothing here.
    if (p.scroll === 0 && (moved || card.boxH !== card.body.clientHeight)) {
      card.boxH = card.body.clientHeight;
      followBottom(card);
    }
  }

  return visible.map((w) => w.id);
}

/**
 * Bring a pane's screen to `lines`, touching only the rows that changed:
 * a spinner repaints one row, not the screen. Rows are one `<span>` each,
 * joined by newlines, so the text and its selection read as before.
 */
function paint(card, lines, shift = 0) {
  let was = card.lines;
  card.lines = lines;
  if (was.length !== lines.length) {
    card.screen.innerHTML = lines.map((line) => `<span>${line}</span>`).join('\n');
    return true;
  }
  const screen = card.screen;
  if (shift > 0 && shift < lines.length) {
    // The screen scrolled: move the row nodes up instead of rewriting them.
    // Each row is a span and the newline after it.
    for (let i = 0; i < shift; i++) {
      screen.firstChild.remove();
      screen.firstChild.remove();
    }
    for (let i = 0; i < shift; i++) screen.append('\n', document.createElement('span'));
    was = was.slice(shift).concat(Array(shift).fill(''));
  }
  const rows = screen.children;
  let moved = shift > 0;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i] === was[i]) continue;
    rows[i].innerHTML = lines[i];
    moved = true;
  }
  return moved;
}

function buildPaneCard(win, p) {
  const color = tint(win);
  const dot = h('span', { class: `dot ${win.state || 'idle'}` });
  const nameNode = h('b', { class: 'pane-name', text: win.name });
  const metaNode = h('span', { class: 'pane-meta mono' });
  // Pressing it must not take focus from the input that `claim` just gave it.
  const takeBtn = h('button', { class: 'pane-tool', 'data-cmd': 'claim', onmousedown: (e) => e.preventDefault() }, 'Take control');
  const findBtn = h('button', { class: 'pane-tool icon', 'data-cmd': 'find', title: 'Search scrollback' }, sym('search'));
  // Prompt marks (OSC 133) the shell sent: hop between the commands it ran.
  const prevBtn = h('button', { class: 'pane-tool icon', 'data-cmd': 'prompt-prev', title: 'Previous prompt' }, '↑');
  const nextBtn = h('button', { class: 'pane-tool icon', 'data-cmd': 'prompt-next', title: 'Next prompt' }, '↓');
  const zoomBtn = h('button', { class: 'pane-tool icon', 'data-cmd': 'zoom', title: 'Zoom' }, sym('zoom'));
  const closeBtn = h('button', { class: 'pane-tool icon', 'data-cmd': 'close', title: 'Close' }, sym('close'));

  // "↓ 240 back to live" floats over the output rather than living in the
  // head: on a phone there is no head, and it is the one control that
  // matters while reading.
  const pill = h(
    'button',
    { class: 'pane-live mono', title: 'Back to the live output', hidden: true, onclick: (e) => {
      e.stopPropagation();
      // Two scrolls, not one: tmux drops its history offset, and the box
      // this pane is drawn in keeps whatever the finger left it at.
      scrolls.delete(win.id);
      send({ kind: 'scroll-bottom', window: win.id });
      const card = paneCache.get(win.id);
      if (card) followBottom(card);
    } },
  );

  const head = h(
    'div',
    { class: 'pane-head' },
    dot,
    nameNode,
    metaNode,
    h('span', { class: 'grow' }),
    takeBtn,
    prevBtn,
    nextBtn,
    findBtn,
    zoomBtn,
    closeBtn,
  );

  const screen = h('pre', { class: 'screen' });
  const body = h('div', { class: 'pane-body' }, screen);
  const root = h(
    'div',
    { class: 'pane', 'data-id': String(win.id), style: color ? `border-color: ${color};` : null },
    head,
    body,
    pill,
  );

  bindTap(
    head,
    (e) => {
      const button = e.target.closest('[data-cmd]');
      if (!button) return;
      // The head's own buttons are not a tap on the terminal.
      e.stopPropagation();
      switch (button.dataset.cmd) {
        case 'close':
          send({ kind: 'close', window: win.id });
          break;
        case 'find':
          findInOutput(win.id);
          break;
        case 'zoom':
          state.zoomed = state.zoomed === win.id ? null : win.id;
          emit();
          break;
        case 'prompt-prev':
        case 'prompt-next':
          send({ kind: 'prompt', window: win.id, delta: button.dataset.cmd === 'prompt-prev' ? -1 : 1 });
          break;
        case 'claim':
          claim(win.id);
          focusInput();
          break;
      }
    },
    // Zooming and taking control answer on the press; closing and searching wait for the click.
    (e) => ['zoom', 'claim'].includes(e.target.closest('[data-cmd]')?.dataset.cmd),
  );

  return {
    el: root,
    head,
    dot,
    nameNode,
    metaNode,
    body,
    screen,
    takeBtn,
    pill,
    name: win.name,
    lines: [],
    cols: p?.cols || 80,
    rows: p?.rows || 24,
    stateClass: win.state || 'idle',
  };
}

/**
 * Echo the staged line at the cursor.
 *
 * Only where there is a cursor to echo at: the desktop keeps one live
 * emulator, for the pane it is focused on, and that is the only screen
 * whose HTML marks the cell. Every other pane comes from `capture-pane`,
 * which has no cursor and is padded with blank rows - guessing at the last
 * printed glyph put the line inside a TUI's status bar, which is worse
 * than not drawing it, and the composer below the pane says the same thing
 * honestly.
 *
 * Nothing is sent either way. This is the local half of typing, and it is
 * why a phone feels instant.
 */
function paintGhost() {
  const card = paneCache.get(shown());
  if (!card) return;
  if (card.ghost) {
    card.ghost.remove();
    card.ghost = null;
  }
  const line = state.staged;
  if (!line || !card.mine) return;
  const cursor = card.screen.querySelector('.cur');
  if (!cursor) return;
  card.ghost = h('span', { class: 'ghost', text: line });
  cursor.before(card.ghost);
  if (card.body.scrollHeight > card.body.clientHeight) {
    card.body.scrollTop = card.body.scrollHeight;
  }
}

// ---------- local echo ----------

// A character typed into a shell comes back from the desktop a round trip
// later. Until then it is drawn, dim, in front of the cursor: `card.pred`
// is the guess, `card.predEl` its node, and `predRow`/`predCol`/`predBase`
// where the cursor was and what its row said when the first one went in.
// The desktop's next version of that row replaces the node, and the
// cursor's advance says how many characters it took; the rest stay.
// Prediction is never made for a full-screen program, a mouse-mode program
// or a scrolled pane. Nothing is shown on a line until the desktop has
// echoed one guess back (`predOk`), so a password prompt never flashes what
// is typed; a guess nothing answers within the round trip plus 300 ms turns
// it off for that pane until the next Enter, as mosh does.

const PRINTABLE = /^[^\x00-\x1f\x7f]+$/;

/** Index of the row `<span>` holding `node`. */
function rowIndex(card, node) {
  while (node.parentNode !== card.screen) node = node.parentNode;
  return Array.prototype.indexOf.call(card.screen.children, node);
}

/** Characters of `row` before the cursor. */
function cursorCol(row, cursor) {
  const range = document.createRange();
  range.setStart(row, 0);
  range.setEndBefore(cursor);
  return range.toString().length;
}

function clearPred(card) {
  card.predEl?.remove();
  card.predEl = null;
  card.pred = '';
  clearTimeout(card.predTimer);
}

function drawPred(card) {
  const cursor = card.screen.querySelector('.cur');
  if (!cursor) return clearPred(card);
  card.predEl ||= h('span', { class: 'predict' });
  card.predEl.textContent = card.pred;
  card.predEl.hidden = !card.predOk;
  cursor.before(card.predEl);
}

function armPred(card) {
  clearTimeout(card.predTimer);
  card.predTimer = setTimeout(() => {
    card.predOff = true;
    clearPred(card);
  }, 300 + (lag.rtt || 0));
}

/** Draw `text`, just typed into window `id`, before the cursor. */
function predict(id, text) {
  const card = paneCache.get(id);
  if (!card) return;
  const p = pane(id);
  const cursor = card.screen.querySelector('.cur');
  if (card.predOff || !card.mine || !p || p.alt || p.mouse || p.scroll > 0 || state.staged || !cursor || !PRINTABLE.test(text)) {
    return clearPred(card);
  }
  if (!card.pred) {
    const row = rowIndex(card, cursor);
    card.predRow = row;
    card.predBase = card.lines[row];
    card.predCol = cursorCol(card.screen.children[row], cursor);
    armPred(card);
  } else if (!card.predEl?.isConnected) {
    return clearPred(card);
  }
  card.pred = (card.pred || '') + text;
  drawPred(card);
}

/** Any key but a printable one: Enter starts over, Backspace takes one back, the rest cancels. */
function predictKey(id, name) {
  const card = paneCache.get(id);
  if (!card) return;
  if (name === 'Enter') card.predOff = card.predOk = false;
  if (name !== 'BSpace') return clearPred(card);
  if (!card.pred) return;
  card.pred = card.pred.slice(0, -1);
  if (card.pred) drawPred(card);
  else clearPred(card);
}

/** After a repaint: take what the desktop echoed off the guess, keep the rest. */
function reconcile(card, p) {
  if (!card.pred || (card.predEl.isConnected && !p.shift)) return;
  const cursor = card.screen.querySelector('.cur');
  if (!cursor || !card.mine || p.alt || p.mouse || p.scroll > 0 || rowIndex(card, cursor) !== card.predRow) {
    return clearPred(card);
  }
  const col = cursorCol(card.screen.children[card.predRow], cursor);
  const took = Math.min(card.pred.length, Math.max(0, col - card.predCol));
  card.pred = card.pred.slice(took);
  card.predCol = col;
  card.predBase = card.lines[card.predRow];
  if (took) card.predOk = true;
  if (!card.pred) return clearPred(card);
  if (took) armPred(card);
  drawPred(card);
}

// ---------- the window strip ----------

/**
 * Every window in this project, one tap away.
 *
 * A phone shows one pane, so switching has to be cheaper than a trip to
 * the sidebar. It lists one project rather than everything: two agents
 * called `claude` in two checkouts are indistinguishable as chips, and
 * "which project" is a rarer question than "which window" - so it gets
 * the first chip and a sheet, not a permanent row.
 */
function renderWins(snap) {
  const strip = el('wins');
  const shownId = shown();
  const isMobile = mobile();

  // A laptop always has a stage; only a phone leaves it for the list.
  if (isMobile && state.view !== 'stage') {
    strip.hidden = true;
    return;
  }

  // The pane on screen decides which project the strip is showing: a
  // window tapped in the sidebar may live in another one.
  const here = windowById(shownId)?.project ?? selected();
  const proj = snap.projects.find((p) => p.id === here);
  const mine = proj?.windows || [];
  const stamp = `${isMobile ? 1 : 0}|${proj?.name}|${mine
    .map((w) => `${w.id}:${w.name}:${w.state}:${w.attention ? 1 : 0}:${w.id === shownId ? 1 : 0}:${driving(w.id) ? 1 : 0}`)
    .join('|')}`;
  strip.hidden = !proj;
  if (strip.dataset.stamp === stamp) return;
  strip.dataset.stamp = stamp;
  if (!proj) return;

  const tabs = [];
  tabs.push(
    h('button', { class: 'tab proj', type: 'button', title: proj.path }, h('span', { class: 'tab-name', text: proj.name })),
  );
  for (const win of mine) {
    const classes = ['tab'];
    if (win.id === shownId) classes.push('on');
    if (win.attention) classes.push('attn');
    const children = [
      h('span', { class: `dot ${win.state || 'idle'}` }),
      h('span', { class: 'tab-name', text: win.name }),
    ];
    if (!isMobile) children.push(h('span', { class: 'tab-x', text: '×' }));
    tabs.push(
      h('button', { class: classes.join(' '), type: 'button', dataset: { id: String(win.id) } }, ...children),
    );
  }
  tabs.push(h('button', { class: 'tab add', type: 'button', title: 'New window' }, '+'));

  strip.replaceChildren(...tabs);
}

/** Fill the stage header. */
function renderStageHead(snap) {
  if (mobile()) return;
  const shownId = shown();
  const win = windowById(shownId);
  const proj = win ? snap.projects.find((p) => p.id === win.project) : null;
  const p = win ? snap.panes.find((pn) => pn.id === win.id) : null;

  const nameEl = el('stage-name');
  const badgeEl = el('stage-badge');
  const subEl = el('stage-sub');

  if (proj) {
    nameEl.textContent = proj.name;
  } else {
    nameEl.textContent = '';
  }

  if (win) {
    badgeEl.textContent = win.alive ? 'LIVE' : 'STOPPED';
    badgeEl.className = win.alive ? 'badge live' : 'badge';

    const parts = [];
    if (proj) parts.push(proj.path.replace(/^\/home\/[^/]+/, '~'));
    if (win.mem_kib) parts.push(bytes(win.mem_kib));
    const aged = age(win);
    if (win.state && !aged) parts.push(win.state);
    const other = heldBy(win.id);
    if (other) parts.push(`driven by ${other}`);
    subEl.replaceChildren(parts.join(' · ') + (aged ? ' · ' : ''), aged ?? '');
  } else {
    badgeEl.textContent = '';
    badgeEl.className = 'badge';
    subEl.textContent = '';
  }

  // Layout buttons.
  for (const btn of el('layout').querySelectorAll('[data-layout]')) {
    btn.classList.toggle('on', btn.dataset.layout === state.layout);
  }
}

/** Which project the strip is listing. Its windows come with it. */
function projectMenu(anchor) {
  menu(
    'Project',
    (state.snap?.projects || []).map((proj) => ({
      label: proj.name,
      hint: `${proj.windows.length} window${proj.windows.length === 1 ? '' : 's'}`,
      run: () => {
        state.selected = proj.id;
        state.shown = proj.windows[0]?.id ?? null;
        emit();
      },
    })),
    anchor,
  );
}

/** New window here: the harnesses the desktop knows about, in a sheet. */
function newWindow() {
  // The project the strip is listing, which is the one the pane on screen
  // belongs to - not whatever the sidebar last highlighted.
  const project = windowById(shown())?.project ?? selected();
  if (!project) return;
  menu('New window', [
    { label: 'Terminal', hint: 'A plain shell', run: () => spawnHere(project) },
    ...(state.snap?.harnesses || []).map((harness) => ({
      label: harness.label,
      hint: harness.id,
      run: () => spawnHere(project, harness.id),
    })),
  ]);
}

/** What the head's buttons are on a laptop, as a sheet for a thumb. */
function windowMenu(anchor) {
  const id = shown();
  const win = windowById(id);
  if (!win) return;
  const p = pane(id);
  menu(
    { title: win.name, subtitle: `${project(win.project)?.name ?? ''} · ${win.state}`, icon: icon(win.icon) },
    [
      driving(id)
        ? { label: 'Give the keyboard back', hint: 'The desktop takes over', run: () => send({ kind: 'release', window: id }) }
        : { label: 'Take control', hint: 'Type into this terminal', run: () => { claim(id); focusInput(); } },
      p && p.scroll > 0 ? { label: 'Back to live output', run: () => send({ kind: 'scroll-bottom', window: id }) } : null,
      { label: 'Previous prompt', hint: 'Needs shell integration', run: () => send({ kind: 'prompt', window: id, delta: -1 }) },
      { label: 'Next prompt', run: () => send({ kind: 'prompt', window: id, delta: 1 }) },
      '-',
      { label: 'Show on desktop', hint: 'Raise + focus', run: () => send({ kind: 'focus', window: id }) },
      { label: 'Paste from clipboard', run: () => pasteInto(id) },
      '-',
      win.alive && { label: 'Find in output', run: () => findInOutput(id) },
      win.alive && { label: 'Copy visible', run: async () => {
        const res = await api(`/api/capture?window=${id}`);
        copy(res.text, 'Copied screen');
      }},
      win.alive && { label: 'Copy last 200 lines', run: async () => {
        const res = await api(`/api/capture?window=${id}&lines=200`);
        copy(res.text, 'Copied 200 lines');
      }},
      !win.alive && { label: 'Last 200 lines', run: () => showTranscript(id) },
      win.alive && { label: 'Copy transcript path', run: async () => {
        try {
          const res = await api(`/api/transcript?window=${id}&lines=1`);
          copy(res.path, 'Copied path');
        } catch (e) {
          toast(e.error || String(e), 'bad');
        }
      }},
      '-',
      { label: 'Interrupt', hint: '^C', run: () => send({ kind: 'interrupt', window: id }) },
      { label: 'Restart', run: () => send({ kind: 'restart', window: id }) },
      ...windowItems(win),
      win.tmux && { label: 'Copy tmux command', hint: 'ssh, then paste', run: () => copy(win.tmux) },
      {
        label: 'Rename',
        run: async () => {
          const name = await ask('Rename window', win.name);
          if (name) send({ kind: 'rename', window: id, name });
        },
      },
      { label: 'Close', danger: true, run: () => send({ kind: 'close', window: id }) },
    ],
    anchor,
  );
}

async function findInOutput(id) {
  const needle = await ask('Search scrollback');
  if (!needle) return;
  try {
    // The server reads the pane's whole history; the page gets only hits.
    const res = await api(`/api/search?window=${id}&q=${encodeURIComponent(needle)}`);
    if (res.total === 0) {
      toast(`No matches in ${res.lines} lines`, 'warn');
      return;
    }
    // Nodes, not markup: pane output is arbitrary text, and `h()` takes
    // `html`, not `innerHTML`, so the old string built nothing at all.
    const body = h('pre', { class: 'diff' });
    const lower = needle.toLowerCase();
    for (const hit of res.matches) {
      let rest = hit.text;
      body.append(`${hit.line}: `);
      while (rest) {
        const at = rest.toLowerCase().indexOf(lower);
        if (at < 0) break;
        body.append(rest.slice(0, at), h('mark', { text: rest.slice(at, at + needle.length) }));
        rest = rest.slice(at + needle.length);
      }
      body.append(`${rest}\n`);
    }
    const more = res.total > res.matches.length ? `, newest ${res.matches.length}` : '';
    el('modal-card').replaceChildren(
      h('header', { class: 'mono' }, `${res.total} match${res.total === 1 ? '' : 'es'}${more} for ${needle}`),
      body,
      h('footer', {},
        h('button', { class: 'accent', text: 'Close', onclick: () => el('modal').hidden = true }),
      ),
    );
    el('modal').hidden = false;
  } catch (e) {
    toast(e.message || String(e), 'bad');
  }
}

function showTranscript(id) {
  const win = windowById(id);
  if (!win) return;
  api(`/api/transcript?window=${id}&lines=200`).then(res => {
    const pre = h('pre', { class: 'mono transcript', text: res.text });
    el('modal-card').replaceChildren(
      h('header', { class: 'mono', text: win.name }),
      pre,
      h('footer', {},
        h('button', { text: 'Copy', onclick: () => copy(res.text, 'Copied transcript') }),
        h('button', { class: 'accent', text: 'Close', onclick: () => el('modal').hidden = true }),
      ),
    );
    el('modal').hidden = false;
  }).catch(e => {
    toast(e.error || String(e), 'bad');
  });
}

// ---------- keys ----------

function translateKey(e) {
  const key = e.key;
  const ctrl = e.ctrlKey;
  const alt = e.altKey;
  const shift = e.shiftKey;
  const meta = e.metaKey;

  // Printable characters without modifiers: let beforeinput handle them.
  if (key.length === 1 && !ctrl && !alt && !meta) {
    return null;
  }

  const named = {
    Enter: 'Enter',
    Tab: 'Tab',
    Escape: 'Escape',
    Backspace: 'BSpace',
    Delete: 'DC',
    ArrowUp: 'Up',
    ArrowDown: 'Down',
    ArrowLeft: 'Left',
    ArrowRight: 'Right',
    Home: 'Home',
    End: 'End',
    PageUp: 'PPage',
    PageDown: 'NPage',
    Insert: 'IC',
    F1: 'F1',
    F2: 'F2',
    F3: 'F3',
    F4: 'F4',
    F5: 'F5',
    F6: 'F6',
    F7: 'F7',
    F8: 'F8',
    F9: 'F9',
    F10: 'F10',
    F11: 'F11',
    F12: 'F12',
  };

  let name = named[key];

  if (ctrl && key.length === 1) {
    const lower = key.toLowerCase();
    if (lower >= 'a' && lower <= 'z') {
      name = `C-${lower}`;
    } else if (key === ' ') {
      name = 'C-Space';
    }
  }

  if (!name) return null;

  if (shift && (key.startsWith('Arrow') || key.startsWith('F') || ['Home', 'End', 'PageUp', 'PageDown'].includes(key))) {
    name = `S-${name}`;
  }
  if (alt && !ctrl) {
    name = `M-${name}`;
  }

  return { kind: 'key', name };
}

function eventToCell(e, paneEl) {
  const screen = paneEl.querySelector('.screen');
  if (!screen) return { col: null, row: null };

  const rect = screen.getBoundingClientRect();
  const { w, h } = cell();
  const col = Math.floor((e.clientX - rect.left) / w);
  const row = Math.floor((e.clientY - rect.top) / h);

  const p = pane(parseInt(paneEl.dataset.id, 10));
  if (!p || col < 0 || row < 0 || col >= p.cols || row >= p.rows) {
    return { col: null, row: null };
  }
  return { col, row };
}
