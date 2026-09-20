// The terminal side of the page: the panes, the grid they are drawn at, and
// the pointer and hardware-keyboard paths into them. Everything a thumb
// types with lives in `compose.js`.

import {
  state,
  on,
  onStaged,
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
} from './store.js';
import { focusInput } from './compose.js';

// Client-side font scale, persisted locally. Never sent to the server.
let scale = parseFloat(localStorage.getItem('taix-font-scale') || '1.0');

// Measure a monospace cell once to set the base font size.
let cellWidth = 0;
let cellHeight = 0;

// Cache of pane cards by window id: { el, html, cols, rows }.
const paneCache = new Map();

// Composing flag to avoid double-send on hardware input methods.
let composing = false;

export function mountTerm() {
  const grid = el('grid');
  const sink = el('sink');

  measureCell();
  trackViewport();
  applyFont();

  let lastWatchList = [];

  // Watch: send the visible window set on change and every 10s, so the
  // desktop keeps capturing panes it is not itself looking at.
  function sendWatch(visible) {
    const ids = [...new Set([...visible, ...state.snap.lock.remote])].sort((a, b) => a - b);
    const key = ids.join(',');
    if (key !== lastWatchList.join(',')) {
      send({ kind: 'watch', client, windows: ids });
      lastWatchList = ids;
    }
  }

  setInterval(() => {
    if (lastWatchList.length > 0) {
      send({ kind: 'watch', client, windows: lastWatchList });
    }
  }, 10000);

  // Layout buttons.
  for (const btn of el('layout').querySelectorAll('[data-layout]')) {
    btn.addEventListener('click', () => {
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
    if (mobile()) return;
    if (!driving(id)) claim(id);
    focusInput();
  });

  // The window strip and the header's window menu: mobile navigation.
  el('wins').addEventListener('click', (e) => {
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
  });
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
      send({ kind: 'keys', window: id, text: e.data });
    } else if (type === 'insertLineBreak' || type === 'insertParagraph') {
      send({ kind: 'key', window: id, name: 'Enter' });
    } else if (type === 'deleteContentBackward') {
      send({ kind: 'key', window: id, name: 'BSpace' });
    } else if (type === 'deleteWordBackward') {
      send({ kind: 'key', window: id, name: 'C-w' });
    } else if (type === 'deleteContentForward') {
      send({ kind: 'key', window: id, name: 'DC' });
    } else if (type === 'insertFromPaste' || type === 'insertFromPasteAsQuotation') {
      const text = e.dataTransfer?.getData('text') || e.data || '';
      if (text) send({ kind: 'type', window: id, text });
      else handled = false;
    } else {
      handled = false;
    }

    if (handled) {
      e.preventDefault();
      sink.value = '';
    }
  });

  sink.addEventListener('compositionstart', () => {
    composing = true;
  });

  sink.addEventListener('compositionend', (e) => {
    composing = false;
    const id = shown();
    if (id && driving(id) && e.data) {
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
    const id = shown();
    if (!id || !driving(id)) return;
    const cmd = translateKey(e);
    if (cmd) {
      // Cancels the key's own default action too, so no `beforeinput`
      // follows it and the same keystroke is not sent a second time.
      e.preventDefault();
      send({ ...cmd, window: id });
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
    if (!p || !p.mouse || !driving(id)) return;

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

  // Touch scroll. tmux owns the scrollback, so a drag is a request, not a
  // viewport move: the deltas are accumulated and sent once per frame,
  // because a thumb produces touchmove far faster than the session can
  // answer and a queue of them scrolls past where the finger stopped.
  let touch = null;
  grid.addEventListener('touchstart', (e) => {
    if (e.touches.length !== 1) return;
    const paneEl = e.target.closest('.pane');
    if (!paneEl) return;
    touch = { y: e.touches[0].clientY, id: parseInt(paneEl.dataset.id, 10) };
    // A finger on the output while the composer has the keyboard is a
    // scroll, not a "stop typing": left to itself iOS would blur the field
    // and drop the keyboard, which is the single most annoying thing a
    // terminal on a phone can do. The pane's own buttons keep their tap.
    const typing = document.activeElement === el('entry-text');
    if (typing && !e.target.closest('button')) e.preventDefault();
  }, { passive: false });

  grid.addEventListener('touchmove', (e) => {
    if (!touch || e.touches.length !== 1) return;
    const p = pane(touch.id);
    if (!p || p.mouse) return;
    // The content follows the finger: dragging down reveals what was above,
    // which is a step back into the history.
    const dy = e.touches[0].clientY - touch.y;
    const rows = Math.trunc(dy / cell().h);
    if (rows === 0) return;
    touch.y -= rows * cell().h;
    queueScroll(touch.id, rows);
  }, { passive: true });

  grid.addEventListener('touchend', () => {
    touch = null;
  }, { passive: true });

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
    // Landscape with the keyboard up leaves a hundred pixels of terminal;
    // the second key row is the first thing to go.
    doc.classList.toggle('short', vv.height < 460);
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
  const probe = h('pre', { style: 'position:absolute;left:-9999px;top:0;font-family:"JetBrains Mono",ui-monospace,monospace;font-size:16px;line-height:1.3;margin:0;padding:0;white-space:pre;' }, 'M'.repeat(100));
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

// The grid last asked for, and the frame that asks. A resize reflows every
// line of scrollback in tmux, so it waits for the rotation or the pinch to
// finish.
let sentGrid = '';
let gridTimer = null;
let gridFrame = null;

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
    const id = shown();
    if (!id || !driving(id)) return;
    const card = paneCache.get(id);
    if (!card) return;
    const body = card.body;
    if (!body || !body.clientWidth) return;

    const pad = card.pad || (card.pad = padding(body));
    const width = body.clientWidth - pad.x;
    const height = body.clientHeight - pad.y;
    const { w, h } = cell();
    const cols = Math.max(20, Math.floor(width / w));
    const rows = Math.max(4, Math.floor(height / h));

    const key = `${id}:${cols}x${rows}`;
    if (key === sentGrid) return;
    sentGrid = key;
    clearTimeout(gridTimer);
    gridTimer = setTimeout(() => send({ kind: 'grid', window: id, cols, rows }), 150);
  });
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

  let visible = [];
  if (isMobile && state.view === 'home') {
    // Home is the list, not the terminal: nothing is drawn, so the desktop
    // stops capturing for this page too.
  } else if (isMobile) {
    // One pane, and it may live in a project this page is not listing:
    // tapping a window in the strip is how you get there.
    const win = windowById(shownId);
    if (win) visible = [win];
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

  empty.hidden = visible.length > 0;
  grid.hidden = visible.length === 0;
  grid.dataset.layout = state.layout || 'grid';

  const wids = visible.map((w) => w.id).join(',');
  if (grid.dataset.windows !== wids) {
    grid.dataset.windows = wids;
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

    if (card.html !== p.html) {
      card.screen.innerHTML = p.html;
      card.html = p.html;
      card.ghost = null;
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
    card.takeBtn.textContent = mine ? 'Reading' : 'Take control';
    card.el.classList.toggle('mine', mine);
    card.el.classList.toggle('zoomed', state.zoomed === win.id);

    // A fresh claim has to state the grid again even when this page's own
    // size never changed: the desktop's widget owned it in between.
    if (card.mine !== mine) {
      card.mine = mine;
      sentGrid = '';
    }

    // A grid one row taller than the box hides the last line, which is
    // the only one that matters. Until the resize lands, follow the
    // bottom - while reading as much as while typing, or "back to live"
    // lands on a view the finger left halfway up.
    if (p.scroll === 0 && card.body.scrollHeight > card.body.clientHeight) {
      card.body.scrollTop = card.body.scrollHeight;
    }
  }

  return visible.map((w) => w.id);
}

function buildPaneCard(win, p) {
  const color = tint(win);
  const dot = h('span', { class: `dot ${win.state || 'idle'}` });
  const nameNode = h('b', { class: 'pane-name', text: win.name });
  const metaNode = h('span', { class: 'pane-meta mono' });
  const takeBtn = h('button', { class: 'pane-tool', 'data-cmd': 'claim' }, 'Take control');
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
      if (card) card.body.scrollTop = card.body.scrollHeight;
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
    zoomBtn,
    closeBtn,
  );

  const screen = h('pre', { class: 'screen' });
  if (p) screen.innerHTML = p.html;
  const body = h('div', { class: 'pane-body' }, screen);
  const root = h(
    'div',
    { class: 'pane', 'data-id': String(win.id), style: color ? `border-color: ${color};` : null },
    head,
    body,
    pill,
  );

  head.addEventListener('click', (e) => {
    const button = e.target.closest('[data-cmd]');
    if (!button) return;
    // The head's own buttons are not a tap on the terminal.
    e.stopPropagation();
    switch (button.dataset.cmd) {
      case 'close':
        send({ kind: 'close', window: win.id });
        break;
      case 'zoom':
        state.zoomed = state.zoomed === win.id ? null : win.id;
        emit();
        break;
      case 'claim':
        claim(win.id);
        focusInput();
        break;
    }
  });

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
    html: p ? p.html : '',
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
    if (win.state) parts.push(win.state);
    subEl.textContent = parts.join(' · ');
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
      '-',
      { label: 'Show on desktop', hint: 'Raise + focus', run: () => send({ kind: 'focus', window: id }) },
      { label: 'Paste from clipboard', run: async () => {
        try {
          const text = await navigator.clipboard.readText();
          if (text) send({ kind: 'paste', window: id, text });
          else toast('Clipboard is empty', 'warn');
        } catch (e) {
          toast('Clipboard access denied', 'bad');
        }
      }},
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
  const needle = await ask('Find in output');
  if (!needle) return;
  try {
    const res = await api(`/api/capture?window=${id}&lines=2000`);
    const lines = res.text.split('\n');
    const matches = [];
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].toLowerCase().includes(needle.toLowerCase())) {
        matches.push(`${i + 1}: ${lines[i]}`);
      }
    }
    if (matches.length === 0) {
      toast('No matches', 'warn');
      return;
    }
    // Nodes, not markup: pane output is arbitrary text, and `h()` takes
    // `html`, not `innerHTML`, so the old string built nothing at all.
    const body = h('pre', { class: 'diff' });
    const lower = needle.toLowerCase();
    for (const line of matches) {
      let rest = line;
      while (rest) {
        const at = rest.toLowerCase().indexOf(lower);
        if (at < 0) break;
        body.append(rest.slice(0, at), h('mark', { text: rest.slice(at, at + needle.length) }));
        rest = rest.slice(at + needle.length);
      }
      body.append(`${rest}\n`);
    }
    el('modal-card').replaceChildren(
      h('header', { class: 'mono' }, `${matches.length} match${matches.length === 1 ? '' : 'es'} for ${needle}`),
      body,
      h('footer', {},
        h('button', { class: 'accent', text: 'Close', onclick: () => el('modal').hidden = true }),
      ),
    );
    el('modal').hidden = false;
  } catch (e) {
    toast(e.error || String(e), 'bad');
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
