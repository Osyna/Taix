import {
  state,
  on,
  emit,
  send,
  spawnHere,
  h,
  menu,
  ask,
  confirm,
  copy,
  projects,
  project,
  windowById,
  windows,
  view,
  icon,
  sym,
  bytes,
  tint,
  selected,
  folded,
  shown,
  driving,
  claim,
  api,
  el,
  toast,
  ago,
} from './store.js';

let lastSignature = '';
let lastHomeSignature = '';

export function mountSide() {
  const filter = document.getElementById('filter');
  const addProject = document.getElementById('add-project');
  const add = document.getElementById('add');
  const homeTop = document.getElementById('home-top');
  const container = document.getElementById('projects');

  // Filter input
  filter.addEventListener('input', () => {
    state.filter = filter.value;
    emit();
  });

  // Add project button
  addProject.addEventListener('click', async () => {
    const path = await ask('Add project', '', '/absolute/path');
    if (path) send({ kind: 'add-project', path });
  });

  // New window button (for selected project)
  add.addEventListener('click', (e) => {
    const snap = state.snap;
    if (!snap || !snap.selected) return;
    openHarnessMenu(snap.selected, e.currentTarget);
  });

  // Subscribe to snapshot updates
  on(() => {
    renderHomeTop(homeTop);
    render(container);
  });
}

function renderHomeTop(container) {
  const snap = state.snap;
  if (!snap) return;
  
  const attnWins = windows().filter((w) => w.attention);
  const allWins = windows();
  const liveCount = allWins.filter((w) => w.alive).length;
  const agentCount = allWins.filter((w) => w.kind !== 'terminal').length;
  
  // signature for home-top only
  const sig = [
    attnWins.map((w) => w.id).join(','),
    state.tab,
    allWins.length,
    liveCount,
    agentCount,
  ].join('|');
  
  if (sig === lastHomeSignature) return;
  lastHomeSignature = sig;
  
  const parts = [];
  
  // attention card
  if (attnWins.length > 0) {
    const names = attnWins.slice(0, 3).map((w) => w.name).join(', ');
    const whoText = attnWins.length > 3 ? `${names}, +${attnWins.length - 3} more` : names;
    parts.push(
      h(
        'div',
        { class: 'attn-card' },
        h('div', { class: 'attn-head' }, h('span', { class: 'dot waiting' }), h('b', { text: `${attnWins.length} terminal${attnWins.length > 1 ? 's' : ''} need you` })),
        h('div', { class: 'attn-who mono', text: whoText }),
        h('button', {
          class: 'accent attn-go',
          text: 'Open & answer',
          onclick: () => {
            const win = attnWins[0];
            state.shown = win.id;
            state.selected = win.project;
            state.zoomed = null;
            view('stage');
            claim(win.id);
            el('entry-text').focus();
          },
        }),
      ),
    );
  }
  
  // filter chips
  const tabs = [
    { id: 'all', label: `All ${allWins.length}` },
    { id: 'live', label: `Live ${liveCount}` },
    { id: 'recent', label: 'Recent' },
    { id: 'agents', label: `Agents ${agentCount}` },
  ];
  parts.push(
    h(
      'div',
      { class: 'chips' },
      ...tabs.map((tab) =>
        h('button', {
          class: `chip-tab${state.tab === tab.id ? ' on' : ''}`,
          text: tab.label,
          onclick: () => {
            state.tab = tab.id;
            emit();
          },
        }),
      ),
    ),
  );
  
  container.replaceChildren(...parts);
}

function render(container) {
  const snap = state.snap;
  if (!snap) return;

  const filterText = state.filter.toLowerCase();
  const selId = selected();
  const shownId = shown();
  const remoteIds = snap.lock.remote;

  // Build signature: only re-render when visible data actually changed
  const sig = snap.projects
    .map((p) => {
      const matchingWindows = p.windows.filter((w) =>
        filterText ? w.name.toLowerCase().includes(filterText) : true,
      );
      if (filterText && matchingWindows.length === 0) return '';
      return [
        p.id,
        p.name,
        p.path,
        folded(p.id) ? '1' : '0',
        p.windows
          .map((w) => `${w.id}:${w.name}:${w.state}:${w.icon}:${w.tint || ''}:${w.alive}:${w.attention}:${w.branch || ''}:${w.mem_kib || ''}:${w.cpu || ''}`)
          .join(','),
        selId === p.id ? 'sel' : '',
        shownId,
        remoteIds.join(','),
        state.tab,
      ].join('|');
    })
    .join(';');

  if (sig === lastSignature) return;
  lastSignature = sig;
  
  let cards = [];
  
  if (state.tab === 'recent') {
    // flatten all windows, sort by most recent non-idle
    const allWins = windows().filter((w) =>
      filterText ? w.name.toLowerCase().includes(filterText) : true,
    );
    // alive windows first, then by name
    const sorted = allWins.sort((a, b) => {
      if (a.alive !== b.alive) return b.alive - a.alive;
      return a.name.localeCompare(b.name);
    });
    if (sorted.length > 0) {
      const pseudoProj = { id: 'recent', name: 'Recent', path: '', windows: [] };
      const card = h('div', { class: 'proj' });
      card.append(
        h('div', { class: 'proj-head' }, h('div', { class: 'proj-info' }, h('div', { class: 'proj-name', text: 'Recent' }))),
        ...sorted.map((w) => renderWindow(w, w.project)),
      );
      cards.push(card);
    }
  } else {
    // normal project view
    cards = snap.projects
      .map((p) => {
        let matchingWindows = p.windows.filter((w) =>
          filterText ? w.name.toLowerCase().includes(filterText) : true,
        );
        // apply tab filter
        if (state.tab === 'live') {
          matchingWindows = matchingWindows.filter((w) => w.alive);
        } else if (state.tab === 'agents') {
          matchingWindows = matchingWindows.filter((w) => w.kind !== 'terminal');
        }
        if (matchingWindows.length === 0) return null;
        return renderProject(p, matchingWindows);
      })
      .filter(Boolean);
  }

  container.replaceChildren(...cards);
}

function renderProject(proj, matchingWindows) {
  const isSelected = selected() === proj.id;
  const isFolded = folded(proj.id);
  // A folded project still has to say a browser is typing inside it: the
  // row that would have said so is not on screen.
  const hidden = isFolded && proj.windows.some((w) => driving(w.id));
  const card = h(
    'div',
    { class: `proj${isFolded ? ' folded' : ''}${isSelected ? ' sel' : ''}` },
    h(
      'div',
      {
        class: 'proj-head',
        // The name opens the project: its first window on the stage. The
        // chevron folds; the dots are the menu.
        onclick: (e) => {
          if (e.target.closest('.proj-menu, .chevron')) return;
          state.selected = proj.id;
          state.shown = proj.windows[0]?.id ?? null;
          state.zoomed = null;
          view('stage');
        },
      },
      h('button', {
        class: 'chevron',
        'aria-label': isFolded ? 'Expand' : 'Collapse',
        onclick: (e) => {
          e.stopPropagation();
          if (isFolded) {
            state.folded.delete(proj.id);
          } else {
            state.folded.add(proj.id);
          }
          emit();
        },
        text: isFolded ? '›' : '⌄',
      }),
      h('div', { class: 'proj-info' }, h('div', { class: 'proj-name', text: proj.name }), h('div', { class: 'proj-path mono', text: shortenPath(proj.path) })),
      hidden ? h('span', { class: 'row-remote', title: 'A browser is typing in this project' }) : null,
      h('div', { class: 'proj-count', text: String(proj.windows.length) }),
      h('button', {
        class: 'proj-menu',
        'aria-label': 'Project menu',
        onclick: (e) => {
          e.stopPropagation();
          openProjectMenu(proj, e.currentTarget);
        },
        text: '⋯',
      }),
    ),
  );

  if (!isFolded) {
    const windowRows = matchingWindows.map((w) => renderWindow(w, proj.id));
    const newTermRow = h(
      'button',
      {
        class: 'new-term',
        onclick: () => spawnHere(proj.id),
      },
      '+ New terminal',
    );
    card.append(...windowRows, newTermRow);
  }

  return card;
}

function renderWindow(win, projectId) {
  const isFocused = shown() === win.id;
  const isDriving = driving(win.id);
  
  // subtitle: for browser windows show the URL; for terminals show state/mem
  let subtitle = '';
  if (win.kind === 'browser' && win.url) {
    subtitle = win.url;
  } else if (win.alive) {
    const parts = [];
    if (win.mem_kib) parts.push(bytes(win.mem_kib));
    parts.push(win.state);
    subtitle = parts.join(' · ');
  } else {
    subtitle = win.state;
  }

  // Answer a prompt from the list without opening the pane. Borrow the
  // keyboard for the keystroke and hand it straight back, so the desktop
  // never notices; a page already driving the window keeps driving it.
  const quick = (label, title, cmds) =>
    h('button', {
      class: 'quick',
      title,
      text: label,
      onclick: (e) => {
        e.stopPropagation();
        const borrowed = !driving(win.id);
        if (borrowed) send({ kind: 'takeover', window: win.id });
        for (const cmd of cmds) send({ ...cmd, window: win.id });
        if (borrowed) send({ kind: 'release', window: win.id });
      },
    });
  const quickActions = win.kind !== 'browser' && win.state === 'waiting' && win.alive
    ? h(
        'div',
        { class: 'win-quick' },
        quick('↵', 'Enter', [{ kind: 'key', name: 'Enter' }]),
        quick('y', 'Answer y', [{ kind: 'keys', text: 'y' }, { kind: 'key', name: 'Enter' }]),
        quick('^C', 'Interrupt', [{ kind: 'interrupt' }]),
      )
    : null;

  const row = h(
    'div',
    {
      class: `win${isFocused ? ' focused' : ''}${win.attention ? ' attention' : ''}${isDriving ? ' driving' : ''}`,
      onclick: () => {
        // The stage follows the tap, and so does the project column: a
        // window opened from a project this page was not listing would
        // otherwise show an empty stage.
        state.shown = win.id;
        state.selected = projectId;
        state.zoomed = null;
        view('stage');
      },
    },
    h('div', { class: 'win-icon' }, icon(win.icon)),
    h('div', { class: 'win-info' }, h('div', { class: 'win-name', text: win.name, style: tint(win) ? `color: ${tint(win)}` : '' }), subtitle ? h('div', { class: 'win-subtitle mono', text: subtitle }) : null),
    h('span', { class: `dot ${win.state}` }),
    quickActions,
    h('button', {
      class: 'win-tools',
      'aria-label': 'Window menu',
      onclick: (e) => {
        e.stopPropagation();
        openWindowMenu(win, e.currentTarget);
      },
      text: '⋯',
    }),
  );
  return row;
}

function openWindowMenu(win, anchor) {
  const tints = state.snap?.palette || [];
  const proj = project(win.project);
  
  const items = [
    {
      label: 'Rename',
      hint: win.name,
      run: async () => {
        const name = await ask('Rename window', win.name);
        if (name) send({ kind: 'rename', window: win.id, name });
      },
    },
    {
      label: 'Zoom',
      hint: state.zoomed === win.id ? 'Clear' : null,
      run: () => {
        state.zoomed = state.zoomed === win.id ? null : win.id;
        emit();
      },
    },
    {
      label: 'Recolor',
      hint: win.tint || 'Default',
      run: () => {
        const current = win.tint || null;
        menu(
          'Recolor',
          [
            { label: 'Default', run: () => send({ kind: 'recolor', window: win.id, color: null }) },
            ...tints.map((t) => ({
              label: t.id,
              run: () => send({ kind: 'recolor', window: win.id, color: t.id }),
            })),
          ],
          anchor,
        );
      },
    },
    {
      label: 'Show on desktop',
      run: () => send({ kind: 'focus', window: win.id }),
    },
    '-',
    {
      label: 'Restart',
      run: () => send({ kind: 'restart', window: win.id }),
    },
    {
      label: win.alive ? 'Kill' : 'Start',
      run: () => send({ kind: win.alive ? 'kill' : 'start', window: win.id }),
    },
    '-',
    win.tmux && win.kind !== 'browser' && {
      label: 'Copy tmux command',
      hint: 'ssh, then paste',
      run: () => copy(win.tmux),
    },
    !win.alive && {
      label: 'Last 200 lines',
      run: async () => {
        try {
          const data = await api(`/api/transcript?window=${win.id}&lines=200`);
          const card = el('modal-card');
          card.replaceChildren(
            h('header', {}, h('b', { text: win.name })),
            h('pre', { class: 'mono transcript', text: data.text }),
            h('footer', {},
              h('button', { class: 'accent', text: 'Copy', onclick: () => copy(data.text, 'transcript') }),
              h('button', { text: 'Close', onclick: () => el('modal').hidden = true })
            )
          );
          el('modal').hidden = false;
        } catch (e) {
          toast(e.message || String(e), 'bad');
        }
      },
    },
    win.alive && {
      label: 'Copy transcript path',
      run: async () => {
        try {
          const data = await api(`/api/transcript?window=${win.id}&lines=1`);
          copy(data.path, 'path');
        } catch (e) {
          toast(e.message || String(e), 'bad');
        }
      },
    },
    win.branch && {
      label: 'Merge into base',
      run: async () => {
        if (await confirm('Merge this worktree back into its base branch?', 'Merge')) {
          send({ kind: 'merge', window: win.id });
        }
      },
    },
    '-',
    {
      label: 'Close',
      danger: true,
      run: async () => {
        if (await confirm(`Close ${win.name}?`, 'Close')) {
          send({ kind: 'close', window: win.id });
        }
      },
    },
  ];
  
  menu(
    { title: win.name, subtitle: `${proj?.name || ''} · ${win.state}`, icon: icon(win.icon) },
    items,
    anchor,
  );
}

function openProjectMenu(proj, anchor) {
  const items = [
    {
      label: 'New terminal',
      run: () => spawnHere(proj.id),
    },
    {
      label: 'New window',
      run: () => openHarnessMenu(proj.id, anchor),
    },
    '-',
    {
      label: 'Open folder',
      run: () => send({ kind: 'open-file', path: proj.path }),
    },
    {
      label: 'Rename project',
      run: async () => {
        const name = await ask('Rename project', proj.name);
        if (name) send({ kind: 'rename-project', project: proj.id, name });
      },
    },
    {
      label: 'Copy path',
      run: () => copy(proj.path),
    },
    '-',
    {
      label: 'Mute notifications',
      // ponytail: ProjectView.muted not yet in proto, show without checked state
      run: () => send({ kind: 'mute', project: proj.id }),
    },
    '-',
    {
      label: 'Close all windows',
      run: async () => {
        if (await confirm('Close all windows in this project?', 'Close all')) {
          send({ kind: 'close-all', project: proj.id });
        }
      },
    },
    {
      label: 'Remove project',
      danger: true,
      run: async () => {
        if (await confirm('Remove this project?', 'Remove')) {
          send({ kind: 'remove-project', project: proj.id });
        }
      },
    },
  ];

  menu(proj.name, items, anchor);
}

function openHarnessMenu(projectId, anchor) {
  const harnesses = state.snap?.harnesses || [];
  const items = [
    {
      label: 'Terminal',
      run: () => spawnHere(projectId),
    },
    ...harnesses.map((h) => ({
      label: h.label,
      run: () => spawnHere(projectId, h.id),
    })),
  ];

  menu('New window', items, anchor);
}

function shortenPath(path) {
  return path.replace(/^\/home\/[^/]+/, '~');
}
