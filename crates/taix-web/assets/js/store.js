// Shared state and the primitives every module builds on: the snapshot,
// the command channel, and the four bits of UI that cannot be per-module
// (toast, sheet, modal, panel routing).
//
// No framework. The whole page is one snapshot in, a handful of DOM
// updates out, and the diffing that matters is "did this object change",
// which a JSON string comparison answers for free.

export const state = {
  /** Latest snapshot from the server, or null before the first one. */
  snap: null,
  /**
   * Which screen has the page: home | stage | panel. Home is the project
   * list, full width, no live terminal; it is where `/` lands and what the
   * back button returns to. The hash carries the stage (`#w<id>`), so a
   * phone reloading mid-command comes back to the same pane.
   */
  view: 'home',
  /** Open panel: files | git | jobs | null. */
  panel: null,
  /** Window whose pane the phone is showing; desktop shows the grid. */
  shown: null,
  /** Filter text from the sidebar search box. */
  filter: '',
  /** Which slice of home is on screen: all | live | recent | agents. */
  tab: 'all',
  /**
   * How the stage arranges what it has room for: one pane, two, or as
   * many as fit. Page-local, remembered, and irrelevant on a phone -
   * there is only ever one pane there.
   */
  layout: localStorage.getItem('taix-layout') || 'focus',
  /** Local selected project (page-local navigation). */
  selected: null,
  /** Local folded projects (Set of ids). */
  folded: new Set(),
  /** Local zoomed window (page-local zoom). */
  zoomed: null,
  /**
   * The line sitting in the mobile composer, not yet sent.
   *
   * It lives here rather than in the composer because the pane echoes it:
   * a command you are typing belongs at the prompt you are typing it into,
   * and the pane is drawn by another module.
   */
  staged: '',
  /** Windows this page has claimed (for reconnect re-assertion). */
  held: new Set(),
};

/** Per page load identifier for watch commands. */
export const client = Math.random().toString(36).slice(2);

/** Projects we've seen, to seed folded state from snapshot on first encounter. */
const seenProjects = new Set();

const subscribers = [];
const panelSubscribers = [];
const reconnectSubscribers = [];

const stagedSubscribers = [];

export function on(fn) {
  subscribers.push(fn);
  if (state.snap) fn(state.snap);
}

export function onPanel(fn) {
  panelSubscribers.push(fn);
}

export function onStaged(fn) {
  stagedSubscribers.push(fn);
}
export function onReconnect(fn) {
  reconnectSubscribers.push(fn);
}

/** Called by app.js on reconnect to re-assert claims and re-watch. */
export function reconnected() {
  reconnectSubscribers.forEach((fn) => fn());
}


/**
 * Stage a line. Local only - nothing is sent until the line is, which is
 * what makes typing on a phone instant instead of a round trip per key.
 */
export function stage(text) {
  if (state.staged === text) return;
  state.staged = text;
  for (const fn of stagedSubscribers) fn(text);
}

/** Called by app.js for each snapshot, and by anything changing local view state. */
export function emit() {
  remember();
  for (const fn of subscribers) {
    try {
      fn(state.snap);
    } catch (e) {
      console.error(e);
    }
  }
}

export const el = (id) => document.getElementById(id);

export function h(tag, props = {}, ...kids) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key === 'text') node.textContent = value;
    else if (key === 'html') node.innerHTML = value;
    else if (key === 'dataset') Object.assign(node.dataset, value);
    else if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
    else if (value === true) node.setAttribute(key, '');
    else node.setAttribute(key, value);
  }
  for (const kid of kids.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    node.append(kid.nodeType ? kid : document.createTextNode(String(kid)));
  }
  return node;
}

// ---------- snapshots ----------

/**
 * A window this page asked for, and the windows that already existed when
 * it did, so the new one can be told apart from anything the desktop or
 * another browser opened in the meantime.
 */
let awaited = null;

/**
 * Where this page was last looking. A phone reloads constantly - the tab
 * is evicted, the network drops, the browser is killed by the OS - and
 * coming back to the desktop's project instead of the pane you were
 * typing into is the difference between a tool and a toy.
 */
let where = {};
try {
  where = JSON.parse(localStorage.getItem('taix-where') || '{}');
} catch {
  where = {};
}

// The last thing written, so a snapshot arriving sixty times a minute does
// not touch the disk: where this page is looking changes a handful of
// times an hour. A debounce was worse than useless here - frames arrive
// faster than any sensible delay, so the write never happened at all.
let wrote = '';

function remember() {
  const next = JSON.stringify({ selected: state.selected, shown: state.shown });
  if (next === wrote) return;
  wrote = next;
  localStorage.setItem('taix-where', next);
  // Switching panes on the stage keeps the hash honest without growing
  // the history: one back gesture is home, whatever was shown.
  const hash = state.view === 'stage' && state.shown ? `#w${state.shown}` : '';
  if (hash && hash !== location.hash) history.replaceState(null, '', hash);
}

/** Take one snapshot: seed this page's navigation, adopt what it asked for. */
export function arrive(snap) {
  // Sparse frame: merge changed panes into held state
  if (snap.pane_ids && state.snap) {
    const held = new Map(state.snap.panes.map((p) => [p.id, p]));
    const incoming = new Map(snap.panes.map((p) => [p.id, p]));
    const merged = [];
    for (const id of snap.pane_ids) {
      const pane = incoming.get(id) || held.get(id);
      if (pane) merged.push(pane);
    }
    snap.panes = merged;
  }
  // First snapshot seeds this page's navigation state
  if (!state.snap) {
    state.view = where.view || (mobile() ? 'side' : 'stage');
    state.selected = where.selected;
    if (where.layout) state.layout = where.layout;
  }
  // Adopt a window this page asked for
  if (awaited) {
    const spawned = snap.projects
      .flatMap((p) => p.windows)
      .find((w) => !awaited.before.includes(w.id));
    if (spawned) {
      state.selected = spawned.project;
      location.hash = `#w${spawned.id}`;
      awaited = null;
    }
  }
  state.snap = snap;
  remember();
  emit();
}

/** `/` is home; `#w<id>` is that window on the stage. */
function route() {
  const id = parseInt((location.hash.match(/^#w(\d+)$/) || [])[1], 10);
  const win = windowById(id);
  if (win) {
    state.shown = win.id;
    state.selected = win.project;
    state.view = 'stage';
  } else {
    state.view = 'home';
  }
}

// The back button is how a phone leaves a terminal.
window.addEventListener('hashchange', () => {
  if (!state.snap) return;
  route();
  emit();
});

/**
 * Open a window and show it *here*.
 *
 * The desktop no longer follows a browser's spawn, so this page has to
 * recognise its own: the first window in that project that was not there
 * when the request went out.
 */
export function spawnHere(project, harness) {
  awaited = { project, before: new Set(windows().map((w) => w.id)), at: Date.now() };
  state.selected = project;
  send(harness ? { kind: 'spawn', project, harness } : { kind: 'new-terminal', project });
}

function adopt() {
  if (!awaited) return;
  // A spawn that failed says so in a toast; give up rather than pounce on
  // the next window someone else opens.
  if (Date.now() - awaited.at > 15000) {
    awaited = null;
    return;
  }
  const fresh = windows().find((w) => w.project === awaited.project && !awaited.before.has(w.id));
  if (!fresh) return;
  awaited = null;
  state.shown = fresh.id;
  state.zoomed = null;
  view('stage');
  claim(fresh.id);
}

// ---------- server ----------

// One POST at a time. Two keystrokes fired as parallel `fetch` calls can
// travel on two connections and arrive in the wrong order, which is a
// terminal typing its characters out of sequence - so they queue.
let inFlight = Promise.resolve();

// Commands parked while the desktop is unreachable. NOT persisted: a
// keystroke typed ten minutes ago, against a pane that has moved on, is
// worse than a lost one.
const cmdQueue = [];
const QUEUE_CAP = 500;
// Set by a failed POST and cleared by the first one that succeeds. Without
// it the next keystroke would overtake everything already parked.
let queued = false;
let retry = null;

function post(cmd) {
  return fetch('/cmd', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(cmd),
    keepalive: true,
  });
}

function park(cmd) {
  if (cmdQueue.length >= QUEUE_CAP) {
    cmdQueue.shift();
    toast('Queue full, oldest keystroke dropped', 'bad');
  }
  cmdQueue.push(cmd);
  emit();
  schedule();
  return Promise.resolve();
}

// A POST can fail without the event stream ever dropping - a phone waking
// up beats the reconnect by seconds - so the queue retries on its own
// rather than waiting for a reconnect that may never come.
function schedule() {
  if (retry || !cmdQueue.length) return;
  retry = setTimeout(() => {
    retry = null;
    flushQueue(state.snap);
  }, 1000);
}

/** Fire a command at the desktop. Never awaited by callers that type. */
export function send(cmd) {
  if (queued || document.body.classList.contains('dropped')) {
    return park(cmd);
  }
  inFlight = inFlight.then(() =>
    post(cmd).catch(() => {
      queued = true;
      park(cmd);
    }),
  );
  return inFlight;
}

/** Send what was parked, in order, dropping anything whose target is gone. */
export function flushQueue(snap) {
  if (!cmdQueue.length) return { sent: 0, dropped: 0 };
  const windows = new Set((snap?.projects || []).flatMap((p) => p.windows.map((w) => w.id)));
  const projects = new Set((snap?.projects || []).map((p) => p.id));
  const batch = cmdQueue.splice(0);
  const valid = batch.filter(
    (cmd) =>
      (!cmd.window || windows.has(cmd.window)) && (!cmd.project || projects.has(cmd.project)),
  );
  let failed = false;
  for (const cmd of valid) {
    inFlight = inFlight.then(() =>
      post(cmd).catch(() => {
        failed = true;
        cmdQueue.push(cmd);
      }),
    );
  }
  inFlight = inFlight.then(() => {
    queued = failed;
    if (failed) schedule();
    emit();
  });
  emit();
  return { sent: valid.length, dropped: batch.length - valid.length };
}

/** Queue count for UI display. */
export function queueCount() {
  return cmdQueue.length;
}

/** A read or a write the web server answers by itself (files, git, jobs). */
export async function api(path, body) {
  const res = await fetch(path, body
    ? { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }
    : undefined);
  const data = await res.json().catch(() => ({ error: `${res.status}` }));
  if (!res.ok || data.error) throw new Error(data.error || `${res.status}`);
  return data;
}

// ---------- the lock ----------

/** True when this page is driving the given window. */
export function driving(id) {
  return state.snap && state.snap.lock.remote.includes(id);
}

/** Claim a terminal: send takeover if not already driving it. */
export function claim(id) {
  state.held.add(id);
  if (!driving(id)) send({ kind: 'takeover', window: id });
}

/** Give back one terminal. */
export function drop(id) {
  state.held.delete(id);
  send({ kind: 'release', window: id });
}

/** True when no snapshot yet (page cannot drive anything). */
export function hasSnap() {
  return !!state.snap;
}

/** Track ownership when the snapshot confirms a window this page claimed is now remote-held. */
export function syncHeld(remoteLocked) {
  for (const id of state.held) {
    if (!remoteLocked.includes(id)) state.held.delete(id);
  }
  for (const id of remoteLocked) {
    if (state.held.has(id)) continue;
    // Detect claims by other instances of this client.
    const wasLocal = false;
    if (wasLocal) state.held.add(id);
  }
}
// ---------- lookups ----------

export const projects = () => state.snap?.projects || [];

export function project(id) {
  return projects().find((p) => p.id === id) || null;
}

export function windows() {
  return projects().flatMap((p) => p.windows.map((w) => ({ ...w, project: p.id })));
}

export function windowById(id) {
  return windows().find((w) => w.id === id) || null;
}

/** Local selected project, falling back to the desktop's selected. */
export function selected() {
  const snap = state.snap;
  if (!snap) return null;
  const ids = projects().map((p) => p.id);
  if (state.selected && ids.includes(state.selected)) return state.selected;
  if (snap.selected && ids.includes(snap.selected)) return snap.selected;
  return projects()[0]?.id ?? null;
}

/** True when the given project is folded locally. */
export function folded(projectId) {
  if (!state.snap) return false;
  // Seed from snapshot on first encounter.
  if (!seenProjects.has(projectId)) {
    seenProjects.add(projectId);
    const proj = project(projectId);
    if (proj && proj.folded) state.folded.add(projectId);
  }
  return state.folded.has(projectId);
}

/** The window a phone is showing, falling back to first window of selected project. */
export function shown() {
  const snap = state.snap;
  if (!snap) return null;
  const ids = windows().map((w) => w.id);
  if (state.shown && ids.includes(state.shown)) return state.shown;
  const sel = projects().find((p) => p.id === selected());
  return sel?.windows[0]?.id ?? null;
}

export function pane(id) {
  return state.snap?.panes.find((p) => p.id === id) || null;
}

export function tint(win) {
  const name = win?.tint;
  if (!name) return null;
  const found = (state.snap?.palette || []).find((p) => p.id === name);
  return found ? found.text : null;
}

// ---------- chrome ----------

export function toast(message, kind = '') {
  const node = h('div', { class: `toast ${kind}`, text: message });
  el('toasts').append(node);
  setTimeout(() => node.classList.add('out'), 2600);
  setTimeout(() => node.remove(), 3000);
}

/** Report a thrown error where the user can see it. */
export function failed(e) {
  toast(String(e.message || e), 'bad');
}

/**
 * Put text on the clipboard. `navigator.clipboard` only exists on https
 * and localhost, and a phone reaches this server over plain http on the
 * LAN, so fall back to `execCommand('copy')` on a scratch textarea, and
 * when even that is refused show the text selected in the modal to be
 * copied by hand. Never fails silently: the whole point is the paste.
 */
export async function copy(text, what = 'Copied') {
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      toast(what);
      return;
    }
  } catch {}
  // Text child, not a `value` attribute: a textarea has none. `readonly`
  // keeps the soft keyboard down; iOS ignores `select()` on one, hence the
  // explicit range.
  const scratch = h('textarea', { readonly: true, style: 'position:fixed;top:0;left:0;opacity:0' }, text);
  document.body.append(scratch);
  scratch.focus();
  scratch.setSelectionRange(0, text.length);
  let ok = false;
  try { ok = document.execCommand('copy'); } catch {}
  scratch.remove();
  if (ok) toast(what);
  else await ask('Copy this', text);
}

/**
 * A menu. A bottom sheet on a phone, a floating card on a desktop, one
 * call either way: `[{ label, hint, danger, run }]`.
 *
 * `head` is a string, or `{ title, subtitle, icon }` when the sheet is
 * about one thing and should say which: a phone opens these from a row it
 * has already scrolled past.
 */
export function menu(head, items, anchor) {
  const sheet = el('sheet');
  const card = el('sheet-card');
  const { title, subtitle, icon: mark } = typeof head === 'string' ? { title: head } : head;
  card.replaceChildren(
    h('div', { class: 'sheet-grip', 'aria-hidden': 'true' }),
    h(
      'header',
      { class: 'sheet-head' },
      mark || null,
      h(
        'div',
        { class: 'sheet-head-text' },
        h('b', { text: title }),
        subtitle ? h('span', { class: 'mono', text: subtitle }) : null,
      ),
    ),
    ...items.filter(Boolean).map((item) =>
      item === '-'
        ? h('div', { class: 'sheet-sep' })
        : h(
            'button',
            {
              class: `sheet-item${item.danger ? ' danger' : ''}${item.accent ? ' accent-item' : ''}`,
              onclick: () => {
                close();
                item.run();
              },
            },
            h(
              'span',
              { class: 'sheet-item-text' },
              h('span', { text: item.label }),
              item.note ? h('span', { class: 'note mono', text: item.note }) : null,
            ),
            item.hint ? h('span', { class: 'hint mono', text: item.hint }) : null,
          ),
    ),
    h('button', { class: 'sheet-cancel', text: 'Cancel', onclick: () => close() }),
  );
  const close = () => {
    sheet.hidden = true;
    sheet.onclick = null;
    card.style.removeProperty('left');
    card.style.removeProperty('top');
    card.classList.remove('anchored');
  };
  sheet.hidden = false;
  sheet.onclick = (e) => {
    if (e.target === sheet) close();
  };
  // Anchored to what opened it when there is room; a phone gets the sheet,
  // because a popover under a thumb is a popover you cannot read.
  if (anchor && !mobile()) {
    const box = anchor.getBoundingClientRect();
    card.classList.add('anchored');
    const width = 260;
    card.style.left = `${Math.max(8, Math.min(window.innerWidth - width - 8, box.left))}px`;
    card.style.top = `${Math.min(window.innerHeight - 40, box.bottom + 6)}px`;
  }
  return close;
}

/** One line of input. Resolves to the string, or null when dismissed. */
export function ask(title, value = '', placeholder = '') {
  return new Promise((resolve) => {
    const input = h('input', { value, placeholder, autocomplete: 'off' });
    const done = (result) => {
      el('modal').hidden = true;
      resolve(result);
    };
    el('modal-card').replaceChildren(
      h('header', { class: 'mono', text: title }),
      input,
      h(
        'footer',
        {},
        h('button', { text: 'Cancel', onclick: () => done(null) }),
        h('button', { class: 'accent', text: 'OK', onclick: () => done(input.value) }),
      ),
    );
    el('modal').hidden = false;
    input.focus();
    input.select();
    input.onkeydown = (e) => {
      if (e.key === 'Enter') done(input.value);
      if (e.key === 'Escape') done(null);
    };
  });
}

export function confirm(title, action = 'Do it') {
  return new Promise((resolve) => {
    const done = (result) => {
      el('modal').hidden = true;
      resolve(result);
    };
    el('modal-card').replaceChildren(
      h('header', { class: 'mono', text: title }),
      h(
        'footer',
        {},
        h('button', { text: 'Cancel', onclick: () => done(false) }),
        h('button', { class: 'danger', text: action, onclick: () => done(true) }),
      ),
    );
    el('modal').hidden = false;
  });
}

// ---------- layout ----------

export const mobile = () => window.matchMedia('(max-width: 860px)').matches;

/** Open a panel, or close it by passing the one already open. */
export function panel(name) {
  state.panel = state.panel === name ? null : name;
  if (state.panel && mobile()) state.view = 'panel';
  else if (!state.panel && state.view === 'panel') state.view = 'stage';
  for (const fn of panelSubscribers) fn(state.panel);
  emit();
}

/**
 * Show a screen. The stage writes its window into the hash - a new history
 * entry, so the phone's back gesture returns home; home clears it.
 */
export function view(name) {
  state.view = name;
  const hash = name === 'stage' && shown() ? `#w${shown()}` : '';
  if (hash !== location.hash) {
    if (hash) history.pushState(null, '', hash);
    else history.replaceState(null, '', location.pathname);
  }
  emit();
}

/** Per-window composer drafts in localStorage. */
export const drafts = {
  get(windowId) {
    try {
      const all = JSON.parse(localStorage.getItem('taix-drafts') || '{}');
      return all[windowId] || '';
    } catch {
      return '';
    }
  },
  set(windowId, text) {
    try {
      const all = JSON.parse(localStorage.getItem('taix-drafts') || '{}');
      if (text) all[windowId] = text;
      else delete all[windowId];
      const entries = Object.entries(all);
      if (entries.length > 50) {
        entries.sort((a, b) => (a[1].length || 0) - (b[1].length || 0));
        const keep = Object.fromEntries(entries.slice(-50));
        localStorage.setItem('taix-drafts', JSON.stringify(keep));
      } else {
        localStorage.setItem('taix-drafts', JSON.stringify(all));
      }
    } catch {}
  },
};

export function bytes(kib) {
  if (kib === null || kib === undefined) return '';
  const mb = kib / 1024;
  // Under a tenth of a megabyte, `0.0 MB` says nothing a reader wanted.
  if (mb < 0.1) return `${Math.max(1, Math.round(kib))} KB`;
  return mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
}

export function ago(unix) {
  if (!unix) return 'never';
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - unix));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
}

// ---------- notifications ----------

export const notify = {
  supported() {
    return 'Notification' in window && 'serviceWorker' in navigator;
  },
  enabled() {
    return this.supported() && Notification.permission === 'granted' && localStorage['taix-notify'] === '1';
  },
  async enable() {
    if (!this.supported()) return false;
    try {
      await navigator.serviceWorker.register('/sw.js');
      const perm = await Notification.requestPermission();
      if (perm === 'granted') {
        localStorage['taix-notify'] = '1';
        return true;
      }
    } catch (e) {
      console.error('notify.enable failed', e);
    }
    return false;
  },
  disable() {
    delete localStorage['taix-notify'];
  },
  /** One tap from a menu: on if off, off if on, toast either way. */
  async toggle() {
    if (this.enabled()) {
      this.disable();
      toast('Notifications off');
      return false;
    }
    const ok = await this.enable();
    toast(ok ? 'Notifications on' : 'Notifications refused', ok ? '' : 'bad');
    return ok;
  },
};


export function icon(name, size = 16) {
  return h('img', { class: 'ico', src: `/icons/${name || 'robot'}.svg`, width: size, height: size, alt: '' });
}

/**
 * One of the sprite symbols in index.html. `<use>` beats an `<img>` for
 * anything that has to take the colour of the thing it sits in.
 */
export function sym(name, size = 16) {
  const node = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  node.setAttribute('class', 'ico');
  node.setAttribute('width', size);
  node.setAttribute('height', size);
  node.setAttribute('aria-hidden', 'true');
  const use = document.createElementNS('http://www.w3.org/2000/svg', 'use');
  use.setAttribute('href', `#i-${name}`);
  node.append(use);
  return node;
}

/** How many panes the stage shows at once. Page-local, remembered. */
export function layout(name) {
  state.layout = name;
  localStorage.setItem('taix-layout', name);
  emit();
}
