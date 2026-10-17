import { age } from './age.js';
import { offline, online } from './hints.js';
import * as hostList from './hosts.js';
// Boot: hold the event stream open, publish each snapshot to the modules,
// and own the three pieces of chrome that belong to no panel - the bar, the
// panel tabs, and which column a phone is looking at.

import { state, on, emit, el, arrive, boot, cacheRows, resetStyles, lag, spawnHere, startHere, panel, view, mobile, compact, drawer, shown, selected, toast, copy, windows, windowById, driving, send, claim, reconnected, notify, syncHeld, project, sym, icon, menu, h, flushQueue, queueCount, link, client } from './store.js';
import { mountSide } from './side.js';
import { mountTerm } from './term.js';
import { mountCompose, focusInput } from './compose.js';
import * as lock from './lock.js';
import { device } from './store.js';
import { notice } from './collab.js';

// 44 KB of panel code that a phone session showing only a terminal never
// needs. The desktop's info rail lives in there too, so that layout loads
// it at boot and only a phone pays on first open.
let panels = null;
function loadPanels() {
  if (!panels) {
    panels = import('./panels.js').then((mod) => {
      mod.mountPanels();
      return mod;
    });
  }
  return panels;
}

let socket = null;
let dropped = false;
let lastState = Date.now();
// Reconnect delay: quick after a blip, backing off while the desktop is
// really gone so a sleeping laptop is not hammered.
let backoff = 250;
let again = null;

/** The browser's install offer, held until the user asks for it. */
let install = null;

function installApp() {
  install?.prompt();
  install = null;
}

function stream() {
  if (lock.locked()) return;
  clearTimeout(again);
  // `c` names this page, so the socket sends the screens it watches and
  // nothing else.
  const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws?d=${device}&c=${client}`);
  socket = ws;
  // The style table is per socket: the new one starts numbering again.
  resetStyles();
  ws.onmessage = (ev) => {
    // Not a snapshot: another device handing this page a window.
    if (ev.data.startsWith('{"notice"')) return notice(JSON.parse(ev.data).notice);
    if (ws !== socket) return;
    // `head` when the picture moved, plus the rows of the watched panes
    // that did. A socket always starts with a head.
    const msg = JSON.parse(ev.data);
    if (msg.pong !== undefined) {
      lastPong = performance.now();
      lag.rtt = Math.round(lastPong - msg.pong);
      showLag();
    }
    if (!msg.head && !msg.panes?.length) return;
    if (!msg.head && !state.snap) return;
    lastState = Date.now();
    document.body.classList.remove('booting');
    // A head carries only the sections that changed: lock may be absent.
    if (msg.head?.lock) syncHeld(msg.head.lock.remote, msg.head.lock.holders);
    arrive(msg);
    if (dropped) {
      document.body.classList.remove('dropped');
      dropped = false;
      online();
      const result = flushQueue(state.snap);
      if (result.sent > 0) {
        const note = result.dropped > 0
          ? `Sent ${result.sent} queued keystroke${result.sent > 1 ? 's' : ''}, dropped ${result.dropped}`
          : `Sent ${result.sent} queued keystroke${result.sent > 1 ? 's' : ''}`;
        toast(note);
      } else {
        toast('Reconnected');
      }
    }
    showLag();
  };
  ws.onopen = () => {
    if (ws !== socket) return;
    backoff = 250;
    link.socket = ws;
    if (dropped) {
      for (const id of state.held) {
        send({ kind: 'takeover', window: id });
      }
    }
    // Every socket, not only after a drop: it sends the screens the page
    // watches, and a restarted desktop has never heard of them.
    reconnected();
  };
  ws.onclose = () => {
    // A socket replaced by a newer one says nothing: only the current
    // connection's end is a drop.
    if (ws !== socket) return;
    link.socket = null;
    if (!dropped) {
      document.body.classList.add('dropped');
      dropped = true;
      offline(lastState);
    }
    // The one refusal that is not a network blip is an expired pairing,
    // and the page for that is the pairing page.
    fetch('/health').then((r) => { if (r.status === 401) location.reload(); }, () => {});
    // A device that locked closes with 4423; either way the answer is the
    // lock screen, not a reconnect into a refusal.
    lock.check();
    again = setTimeout(stream, backoff);
    backoff = Math.min(backoff * 2, 5000);
  };
}

let lastPong = 0;

/** Round trip and key-to-screen time, small, in the bar. */
function showLag() {
  const text = lag.rtt === null ? '' : `${lag.rtt} ms${lag.echo === null ? '' : ` · key ${lag.echo} ms`}`;
  const node = el('bar-lag');
  if (node.textContent !== text) node.textContent = text;
}

function ping() {
  if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ kind: 'ping', t: performance.now() }));
}

/** Replace the socket with a fresh one now, not when the old one is noticed to be dead. */
function reconnect() {
  const old = socket;
  stream();
  old.close();
}

setInterval(() => document.visibilityState === 'visible' && ping(), 5000);

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') return cacheRows();
  // Back after more than the idle limit, the server has locked this
  // device; learn it now rather than from the next refused request.
  lock.check();
  if (!socket) return;
  // A phone back from sleep holds a socket the OS killed long ago, and
  // learns so only on its next write: ask, and if nothing answers within
  // a second, reconnect now rather than on that write.
  if (socket.readyState !== WebSocket.OPEN) return reconnect();
  const asked = performance.now();
  ping();
  const asking = socket;
  setTimeout(() => {
    if (lastPong < asked && asking === socket) reconnect();
  }, 1000);
});
window.addEventListener('pagehide', cacheRows);

function bar(snap) {
  const where = snap.bar.find((b) => b.id === 'where');
  el('bar-where').textContent = where ? where.text : '';
  el('bar-facts').textContent = snap.bar
    .filter((b) => b.id !== 'where' && b.text)
    .map((b) => b.text)
    .join('  ·  ');
  const health = snap.health;
  const chip = el('bar-web');
  chip.dataset.life = health.life;
  el('bar-web-text').textContent = health.addr || health.life;
  chip.title = health.error || `${health.clients} watching`;
}

function header(snap) {
  el('host').textContent = snap.host;
  el('notice').textContent = snap.status || '';
  
  const shownWindow = shown();
  const win = windowById(shownWindow);
  const proj = snap.projects.find((p) => p.id === (win?.project ?? selected()));
  
  // mobile crumb: project · window
  el('crumb-name').textContent = win?.name || '';
  const aged = win && age(win);
  const crumb = win ? [proj?.name, aged ? '' : win.state].filter(Boolean).join(' · ') : '';
  el('crumb-sub').replaceChildren(crumb + (aged && crumb ? ' · ' : ''), aged || '');
  
  // facts: N live · M total
  const allWins = windows();
  const liveCount = allWins.filter((w) => w.alive).length;
  el('facts-top').textContent = `${liveCount} live · ${allWins.length} total`;
  
  // attention chip
  const attnWins = allWins.filter((w) => w.attention);
  const attn = el('attn');
  if (attnWins.length > 0) {
    attn.hidden = false;
    el('attn-text').textContent = `${attnWins.length} need${attnWins.length > 1 ? '' : 's'} input`;
  } else {
    attn.hidden = true;
  }

  // App badge: windows whose agent is waiting. Not every browser has one.
  if ('setAppBadge' in navigator) {
    const waiting = allWins.filter((w) => w.state === 'waiting').length;
    (waiting ? navigator.setAppBadge(waiting) : navigator.clearAppBadge()).catch(() => {});
  }
}

function chrome(snap) {
  const doc = document.documentElement;
  doc.dataset.view = state.view;
  doc.dataset.panel = state.panel || '';
  
  // mark active rail/dock buttons: panel open wins over view
  const active = state.panel || state.view;
  for (const button of document.querySelectorAll('#rail button[data-go], #dock button[data-go]')) {
    button.classList.toggle('on', button.dataset.go === active);
  }
  
  el('panel').hidden = !state.panel;
  // desktop nav is the sidebar toggle; on a phone it is the way back out
  // of whatever screen you opened.
  el('nav').classList.toggle('back', mobile() && state.view !== 'home');
}

// Window and project navigation from a hardware keyboard (an iPad's, or a
// laptop's). Command on Apple, Ctrl+Shift elsewhere, where plain Ctrl is the
// terminal's own. Matched on `code`, since Shift changes what `key` spells,
// and only with the modifier held, so Command-C/V/A stay the browser's and
// a bare key still reaches the terminal sink.
const apple = /Mac|iPhone|iPad/.test(navigator.platform);

function shortcut(e) {
  if (!(apple ? e.metaKey && !e.ctrlKey : e.ctrlKey && e.shiftKey && !e.metaKey) || e.altKey) return;
  const win = windowById(shown());
  const proj = win?.project ?? selected();
  const list = project(proj)?.windows || [];
  const at = list.findIndex((w) => w.id === win?.id);
  const code = e.code;

  const to = (target) => {
    if (!target) return;
    state.shown = target.id;
    state.selected = proj;
    state.zoomed = null;
    view('stage');
  };
  if (/^Digit[1-9]$/.test(code)) to(list[+code.slice(5) - 1]);
  else if (code === 'BracketLeft') to(list[at - 1]);
  else if (code === 'BracketRight') to(list[at + 1]);
  else if (code === 'KeyT') {
    if (proj) spawnHere(proj);
  } else if (code === 'Period') {
    if (!win) return;
    claim(win.id);
    send({ kind: 'key', window: win.id, name: 'C-c' });
  } else if (code === 'ArrowUp' || code === 'ArrowDown') {
    const all = state.snap?.projects || [];
    const next = all[all.findIndex((p) => p.id === proj) + (code === 'ArrowUp' ? -1 : 1)];
    if (!next) return;
    state.selected = next.id;
    state.shown = next.windows[0]?.id ?? null;
    state.zoomed = null;
    view('stage');
  } else return;
  e.preventDefault();
}

function openPalette() {
  const snap = state.snap;
  if (!snap) return;
  
  const palette = el('palette');
  const input = el('palette-input');
  const list = el('palette-list');
  
  // build item list: windows, projects, harnesses (for selected project), commands
  const items = [];
  
  // windows
  for (const win of windows()) {
    const proj = project(win.project);
    items.push({
      icon: icon(win.icon),
      label: win.name,
      subtitle: `${proj?.name || ''} · ${win.state}`,
      kind: 'window',
      run: () => {
        state.shown = win.id;
        state.selected = win.project;
        state.zoomed = null;
        view('stage');
      },
    });
  }
  
  // projects
  for (const proj of snap.projects) {
    items.push({
      icon: sym('folder'),
      label: proj.name,
      subtitle: proj.path,
      kind: 'project',
      run: () => {
        state.selected = proj.id;
        state.shown = proj.windows[0]?.id ?? null;
        state.zoomed = null;
        view('stage');
      },
    });
  }
  
  // harnesses (new in selected project)
  const sel = selected();
  if (sel) {
    items.push({
      icon: sym('plus'),
      label: 'New Terminal',
      subtitle: project(sel)?.name || '',
      kind: 'new',
      run: () => spawnHere(sel),
    });
    for (const harness of snap.harnesses || []) {
      items.push({
        icon: sym('plus'),
        label: `New ${harness.label}`,
        subtitle: project(sel)?.name || '',
        kind: 'new',
        run: () => spawnHere(sel, harness.id),
      });
    }
  }
  
  // commands
  const cmds = [
    { label: 'Files', run: async () => { await loadPanels(); panel('files'); } },
    { label: 'Source control', run: async () => { await loadPanels(); panel('git'); } },
    { label: 'Automation', run: async () => { await loadPanels(); panel('jobs'); } },
    { label: 'Ports and preview', run: async () => { await loadPanels(); panel('ports'); } },
    { label: 'Command history', run: async () => { await loadPanels(); panel('history'); } },
    { label: 'Settings', run: async () => { await loadPanels(); panel('settings'); } },
  ];
  const shownWin = windowById(shown());
  if (shownWin?.tmux) {
    cmds.push({ label: 'Copy tmux command', subtitle: shownWin.name, run: () => copy(shownWin.tmux) });
  }
  cmds.push(
    { label: 'Copy page link', run: () => copy(location.origin + '/') },
    { label: 'Give back every terminal', run: () => send({ kind: 'release' }) },
  );
  if (install) cmds.push({ label: 'Install TaiX as an app', run: installApp });
  for (const x of hostList.hosts()) {
    if (x.url !== location.origin) cmds.push({ label: `Switch host: ${x.label}`, subtitle: x.url, run: () => hostList.switchHost(x.url) });
  }
  for (const cmd of cmds) {
    items.push({ icon: sym('gear'), ...cmd, kind: 'command' });
  }
  
  let filtered = items;
  let selIndex = 0;
  
  const render = () => {
    list.replaceChildren(
      ...filtered.map((item, i) =>
        h(
          'button',
          {
            class: `pal-row${i === selIndex ? ' on' : ''}`,
            onclick: () => {
              palette.hidden = true;
              item.run();
            },
          },
          h('div', { class: 'pal-icon' }, item.icon),
          h('div', { class: 'pal-text' }, h('b', { text: item.label }), item.subtitle ? h('span', { class: 'mono', text: item.subtitle }) : null),
          h('span', { class: 'pal-kind mono', text: item.kind }),
        ),
      ),
    );
  };
  
  const filter = () => {
    const q = input.value.toLowerCase();
    filtered = q
      ? items.filter((it) => it.label.toLowerCase().includes(q) || it.subtitle?.toLowerCase().includes(q))
      : items;
    selIndex = 0;
    render();
  };
  
  input.value = '';
  filter();
  palette.hidden = false;
  input.focus();
  
  input.oninput = filter;
  input.onkeydown = (e) => {
    if (e.key === 'Escape') {
      palette.hidden = true;
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      selIndex = Math.min(selIndex + 1, filtered.length - 1);
      render();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      selIndex = Math.max(selIndex - 1, 0);
      render();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (filtered[selIndex]) {
        palette.hidden = true;
        filtered[selIndex].run();
      }
    }
  };
  palette.onclick = (e) => {
    if (e.target === palette) palette.hidden = true;
  };
}

function wire() {
  // nav: back out of whatever the phone opened; the project drawer on a
  // tablet too narrow to keep the list beside the terminal.
  el('nav').onclick = () => {
    if (mobile() && state.view !== 'home') {
      if (state.panel) panel(state.panel);
      else view('home');
    } else {
      drawer();
    }
  };

  // The stage behind an open drawer is its scrim: a tap there closes the
  // drawer and does nothing else, so it cannot also claim a pane.
  el('stage').addEventListener('click', (e) => {
    if (!document.documentElement.classList.contains('drawer')) return;
    e.stopPropagation();
    e.preventDefault();
    drawer(false);
  }, true);
  
  el('panel-close').onclick = async () => { await loadPanels(); panel(state.panel); };
  
  // rail and dock buttons
  for (const button of document.querySelectorAll('#rail button[data-go], #dock button[data-go]')) {
    button.onclick = () => {
      const go = button.dataset.go;
      if (go === 'home' && compact()) {
        drawer();
      } else if (go === 'home' || go === 'stage') {
        view(go);
      } else if (go === 'files' || go === 'git' || go === 'jobs' || go === 'ports' || go === 'history' || go === 'settings') {
        loadPanels().then(() => panel(go));
      } else if (go === 'more') {
        // dock more menu
        menu(
          'More',
          [
            { label: 'Automation', run: async () => { await loadPanels(); panel('jobs'); } },
            { label: 'Ports and preview', run: async () => { await loadPanels(); panel('ports'); } },
            { label: 'Command history', run: async () => { await loadPanels(); panel('history'); } },
            { label: 'Settings', run: async () => { await loadPanels(); panel('settings'); } },
            '-',
            { label: `Notifications ${notify.enabled() ? 'on' : 'off'}`, run: () => notify.toggle() },
            { label: 'Copy page link', run: () => copy(location.origin + '/') },
            { label: 'Give back every terminal', run: () => send({ kind: 'release' }) },
            install && { label: 'Install TaiX as an app', run: installApp },
          ],
          button,
        );
      }
    };
  }
  
  el('bar-web').onclick = () => {
    const health = state.snap?.health;
    const url = health?.url || (health?.addr ? `http://${health.addr}` : null);
    if (url) copy(url, 'Copied pairing link');
  };
  
  el('stage-empty').querySelector('[data-act="new-terminal"]').onclick = () => {
    const win = windowById(shown());
    if (win && !win.alive && win.kind !== 'browser') return startHere(win);
    const projectId = selected();
    if (projectId) spawnHere(projectId);
  };
  
  // attention chip in header
  el('attn').onclick = () => {
    const attnWins = windows().filter((w) => w.attention);
    if (attnWins.length > 0) {
      const win = attnWins[0];
      state.shown = win.id;
      state.selected = win.project;
      state.zoomed = null;
      view('stage');
      claim(win.id);
      focusInput();
    }
  };
  
  // omni button opens palette
  el('omni').onclick = () => openPalette();
  
  // cmd/ctrl+k opens palette (but not when an input has focus)
  document.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
      e.preventDefault();
      openPalette();
      return;
    }
    shortcut(e);
  });
  
  // A page left open on a phone must release all claimed windows.
  window.addEventListener('pagehide', () => {
    navigator.sendBeacon?.('/cmd', new Blob([JSON.stringify({ kind: 'release' })], { type: 'application/json' }));
  });
  
  // A rotation or a keyboard fires this in a burst; one repaint at the end
  // of it is what the page needs.
  let settle = null;
  window.addEventListener('resize', () => {
    clearTimeout(settle);
    settle = setTimeout(() => {
      // The panel is a column on a laptop, not a screen.
      if (!mobile() && state.view === 'panel') view('stage');
      else emit();
    }, 80);
  });
  
  // The page never zooms; the terminal does, by its own pinch (`term.js`).
  // `touch-action` covers modern engines; Safari also zooms from these.
  for (const name of ['gesturestart', 'gesturechange', 'gestureend']) {
    document.addEventListener(name, (e) => e.preventDefault());
  }
}

// Windows that were asking for someone when we last looked, so the flip
// into that state is what speaks - a long wait must not nag.
let asking = new Set();

/**
 * The phone's half of the notification system.
 *
 * A browser on a plain-HTTP LAN address is not a secure context, so the
 * system `Notification` API is unavailable by design: what this page has
 * is the tab title, a toast and the vibrator, and all three are enough to
 * notice an agent stopping to ask something while you are in another app.
 */
async function notices() {
  const now = new Set(windows().filter((w) => w.attention).map((w) => w.id));
  for (const win of windows()) {
    if (now.has(win.id) && !asking.has(win.id)) {
      toast(`${win.name} needs you`, 'warn');
      navigator.vibrate?.([40, 60, 40]);
      // iOS suspends the page when the screen is off.
      if (notify.enabled() && document.visibilityState !== 'visible') {
        const proj = project(win.project);
        const reg = await navigator.serviceWorker.ready;
        reg.showNotification(`${win.name} needs you`, {
          body: proj?.name || '',
          tag: `w${win.id}`,
          renotify: false,
          data: { url: `/#w${win.id}` },
        });
      }
    }
  }
  asking = now;
  document.title = now.size > 0 ? `(${now.size}) TaiX` : 'TaiX';
}

on((snap) => {
  if (!snap) return;
  bar(snap);
  header(snap);
  chrome(snap);
  notices();
});

// Update send button to show queue count when offline
on(() => {
  const count = queueCount();
  const sendBtn = el('entry-send');
  if (!sendBtn) return;
  
  if (count > 0) {
    // Show queue count
    sendBtn.textContent = `${count} queued`;
  } else {
    // Restore the icon
    sendBtn.textContent = '';
    sendBtn.appendChild(sym('send'));
  }
});

// The on-screen key rows are a phone's only Ctrl and symbols, and the
// setting that hides them must apply before the first paint.
for (const part of ['sym', 'ctl']) {
  if (localStorage[`taix-keys-${part}`] === 'off') {
    document.documentElement.setAttribute(`data-keys-${part}`, 'off');
  }
}
wire();
mountSide();
mountTerm();
mountCompose();
chrome(null);
// The head the server wrote into the page, and the rows last shown: the
// first paint needs no socket.
try {
  const head = el('boot');
  if (head) {
    boot(JSON.parse(head.textContent));
    document.body.classList.remove('booting');
  }
} catch (e) {
  console.error(e);
}
stream();
lock.onUnlock(() => {
  backoff = 250;
  const old = socket;
  stream();
  old?.close();
});
lock.check();
if (!mobile()) {
  loadPanels();
}
// Installable and openable offline. Service workers need a secure context:
// over plain http on the LAN this is skipped, and the tailnet's https
// address is where TaiX installs as an app.
if ('serviceWorker' in navigator) {
  navigator.serviceWorker.register('/sw.js').catch((e) => console.error('SW registration failed', e));
}
window.addEventListener('beforeinstallprompt', (e) => {
  e.preventDefault();
  install = e;
});
window.addEventListener('appinstalled', () => {
  install = null;
  toast('TaiX installed');
});
