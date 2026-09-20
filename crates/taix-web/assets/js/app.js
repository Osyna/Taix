// Boot: hold the event stream open, publish each snapshot to the modules,
// and own the three pieces of chrome that belong to no panel - the bar, the
// panel tabs, and which column a phone is looking at.

import { state, on, emit, el, arrive, spawnHere, panel, view, mobile, shown, selected, toast, copy, windows, windowById, driving, send, claim, reconnected, notify, syncHeld, project, sym, icon, menu, h, flushQueue, queueCount } from './store.js';
import { mountSide } from './side.js';
import { mountTerm } from './term.js';
import { mountCompose } from './compose.js';

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

let source = null;
let dropped = false;
let lastState = Date.now();
let lastRaw = '';

function stream() {
  source = new EventSource('/events');
  source.addEventListener('state', (ev) => {
    if (ev.data === lastRaw) return;
    lastRaw = ev.data;
    const snap = JSON.parse(ev.data);
    // Frames only move forward - except across a drop: a restarted server
    // counts from one again, and refusing its frames would freeze the page.
    if (!dropped && state.snap && snap.rev <= state.snap.rev) return;
    lastState = Date.now();
    if (dropped) {
      document.body.classList.remove('dropped');
      dropped = false;
      const result = flushQueue(snap);
      if (result.sent > 0) {
        const msg = result.dropped > 0
          ? `Sent ${result.sent} queued keystroke${result.sent > 1 ? 's' : ''}, dropped ${result.dropped}`
          : `Sent ${result.sent} queued keystroke${result.sent > 1 ? 's' : ''}`;
        toast(msg);
      } else {
        toast('Reconnected');
      }
    }
    document.body.classList.remove('booting');
    syncHeld(snap.lock.remote);
    arrive(snap);
  });
  source.onerror = () => {
    if (!dropped) {
      document.body.classList.add('dropped');
      dropped = true;
    }
    // A stream the server refused stays CLOSED; the one refusal that is
    // not a network blip is an expired pairing, and the page for that is
    // the pairing page.
    if (source.readyState === EventSource.CLOSED) {
      fetch('/health').then((r) => { if (r.status === 401) location.reload(); }, () => {});
    }
  };
  source.onopen = () => {
    if (dropped) {
      for (const id of state.held) {
        send({ kind: 'takeover', window: id });
      }
      reconnected();
    }
  };
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') {
    const stale = Date.now() - lastState > 15000;
    if (source && (source.readyState === EventSource.CLOSED || stale)) {
      source.close();
      stream();
    }
  }
});

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
  el('crumb-sub').textContent = win ? [proj?.name, win.state].filter(Boolean).join(' · ') : '';
  
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
  // nav: back out of whatever the phone opened, sidebar toggle on a laptop
  el('nav').onclick = () => {
    if (mobile() && state.view !== 'home') {
      if (state.panel) panel(state.panel);
      else view('home');
    } else {
      const side = el('side');
      side.hidden = !side.hidden;
    }
  };
  
  el('panel-close').onclick = async () => { await loadPanels(); panel(state.panel); };
  
  // rail and dock buttons
  for (const button of document.querySelectorAll('#rail button[data-go], #dock button[data-go]')) {
    button.onclick = () => {
      const go = button.dataset.go;
      if (go === 'home' || go === 'stage') {
        view(go);
      } else if (go === 'files' || go === 'git' || go === 'jobs' || go === 'settings') {
        loadPanels().then(() => panel(go));
      } else if (go === 'more') {
        // dock more menu
        menu(
          'More',
          [
            { label: 'Automation', run: async () => { await loadPanels(); panel('jobs'); } },
            { label: 'Settings', run: async () => { await loadPanels(); panel('settings'); } },
            '-',
            { label: `Notifications ${notify.enabled() ? 'on' : 'off'}`, run: () => notify.toggle() },
            { label: 'Copy page link', run: () => copy(location.origin + '/') },
            { label: 'Give back every terminal', run: () => send({ kind: 'release' }) },
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
      el('entry-text').focus();
    }
  };
  
  // omni button opens palette
  el('omni').onclick = () => openPalette();
  
  // cmd/ctrl+k opens palette (but not when an input has focus)
  document.addEventListener('keydown', (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
      e.preventDefault();
      openPalette();
    }
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
stream();
if (!mobile()) {
  loadPanels();
}
if (notify.enabled()) {
  navigator.serviceWorker.register('/sw.js').catch((e) => console.error('SW registration failed', e));
}
