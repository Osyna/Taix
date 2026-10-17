import * as hostList from './hosts.js';
import { myKeys, saveMyKeys, attachFile, attachSnippet } from './compose.js';
import * as store from './store.js';
import * as lock from './lock.js';
import { renderHistory } from './history.js';
import { renderPorts } from './preview.js';
import { settingsSection } from './collab.js';

// Per-project current directory for files panel
const dirs = new Map();

// Poll timers (cleared on panel close)
let gitTimer = null;

// Git data cache (shared between panel and info rail)
let gitCache = null;
let gitCachePath = null;
// Jobs for the info rail, refreshed on its timer rather than per frame.
let jobsCache = null;

// Job form state
let jobFormState = null;
// A panel shows files, jobs or settings, none of which change sixty times
// a second - and each render re-fetches. Snapshot-driven refreshes are
// held to one every two seconds; anything the user does calls refreshNow.
let refreshAt = 0;
let refreshTimer = null;
let refreshFn = null;
// A file preview has no live data; re-rendering it would drop the selection.
let previewing = false;
const REFRESH_MS = 2000;

// Commit input reference for git panel
let commitInputRef = null;
let commitMsg = '';
// Git panel: a sub-view (diff) is showing; how to redraw the main view.
let gitSub = false;
let gitReload = null;
let gitRaw = '';
// Previous visit's mark {sha, at} for the open project, and the PR line.
let look = null;
const prBox = store.h('div', { class: 'panel-section', style: 'display:none' });

export function mountPanels() {
  const title = store.el('panel-title');
  const tools = store.el('panel-tools');
  const body = store.el('panel-body');

  // What a snapshot change or a user action re-renders.
  refreshFn = () => {
    const name = store.state.panel;
    if (!name) return;
    const snap = store.state.snap;
    if (!snap) return;
    const proj = store.project(store.selected());
    if (!proj) return;

    if (name === 'files') { if (!previewing) renderFiles(title, tools, body, proj); }
    else if (name === 'jobs') renderJobs(title, tools, body);
    else if (name === 'ports') renderPorts(title, tools, body);
    else if (name === 'settings') renderSettings(title, tools, body);
  };

  store.onPanel((name) => {
    clearTimers();
    clearTimeout(refreshTimer);
    refreshTimer = null;
    refreshAt = Date.now();
    previewing = false;
    if (!name) return;

    const snap = store.state.snap;
    if (!snap) return;

    const proj = store.project(store.selected());
    if (!proj) return;

    if (name === 'files') renderFiles(title, tools, body, proj);
    else if (name === 'git') renderGit(title, tools, body, proj);
    else if (name === 'jobs') renderJobs(title, tools, body);
    else if (name === 'ports') renderPorts(title, tools, body);
    else if (name === 'history') renderHistory(title, tools, body);
    else if (name === 'settings') renderSettings(title, tools, body);
  });

  // Pane rows keep `snap.projects`: only a new head, or another project,
  // is anything a panel could show differently.
  let seen = null;
  store.on((snap) => {
    const key = `${store.selected()}`;
    if (!store.state.panel || refreshTimer || (seen && seen.projects === snap?.projects && seen.key === key)) return;
    seen = { projects: snap?.projects, key };
    const wait = Math.max(0, refreshAt + REFRESH_MS - Date.now());
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      refreshAt = Date.now();
      refreshFn();
    }, wait);
  });

  mountInfo();
}

/** What a user action calls: its own change must show at once. */
export function refreshNow() {
  if (!refreshFn) return;
  clearTimeout(refreshTimer);
  refreshTimer = null;
  refreshAt = Date.now();
  refreshFn();
}


function clearTimers() {
  clearInterval(gitTimer);
  gitTimer = null;
}

// ---------- info rail ----------

function mountInfo() {
  let seen = null;
  store.on((snap) => {
    if (!snap) return;
    const key = `${store.selected()}`;
    if (seen && seen.projects === snap.projects && seen.key === key) return;
    seen = { projects: snap.projects, key };
    renderInfo(snap);
  });

  // Git and jobs for the info rail, on their own clock: neither is in the
  // snapshot, and asking on every frame was twenty requests a second.
  const poll = () => {
    if (document.visibilityState !== 'visible') return;
    const info = store.el('info');
    if (info.offsetParent === null) return;
    fetchJobsForInfo();
    if (store.state.panel === 'git') return; // Panel polls itself
    const proj = store.project(store.selected());
    if (!proj) return;
    fetchGitData(proj.path);
  };
  setInterval(poll, 5000);
  poll();
}

function renderInfo(snap) {
  const info = store.el('info');
  if (info.offsetParent === null) return;

  const proj = store.project(store.selected());
  if (!proj) return;

  const cards = [];

  // SESSION card
  const session = snap.session || {};
  const uptime = formatUptime(session.uptime || 0);
  const link = snap.health?.addr || 'offline';
  cards.push(
    store.h('div', { class: 'info-card' },
      store.h('div', { class: 'panel-label' }, 'SESSION'),
      store.h('div', { class: 'info-row' },
        store.h('span', { class: 'k', text: 'uptime' }),
        store.h('span', { class: 'v mono', text: uptime })
      ),
      store.h('div', { class: 'info-row' },
        store.h('span', { class: 'k', text: 'shell' }),
        store.h('span', { class: 'v mono', text: session.shell || '—' })
      ),
      store.h('div', { class: 'info-row' },
        store.h('span', { class: 'k', text: 'tmux' }),
        store.h('span', { class: 'v mono', text: session.tmux || '—' })
      ),
      store.h('div', { class: 'info-row' },
        store.h('span', { class: 'k', text: 'link' }),
        store.h('span', { class: 'v mono', text: link })
      )
    )
  );

  // MEMORY card
  const projWindows = proj.windows || [];
  const totalMem = projWindows.reduce((sum, w) => sum + (w.mem_kib || 0), 0);
  const maxMem = Math.max(...projWindows.map(w => w.mem_kib || 0), 1);
  const memRows = projWindows.map(w =>
    store.h('div', { class: 'info-row' },
      store.h('span', { class: 'k', text: w.name }),
      store.h('span', { class: 'v mono', text: store.bytes(w.mem_kib || 0) }),
      store.h('div', { class: 'meter' },
        store.h('div', { class: 'meter-fill', style: `width: ${(w.mem_kib / maxMem) * 100}%` })
      )
    )
  );
  cards.push(
    store.h('div', { class: 'info-card' },
      store.h('div', { class: 'panel-label' },
        'MEMORY',
        store.h('span', { class: 'mono', text: store.bytes(totalMem) })
      ),
      ...memRows
    )
  );

  // GIT card
  if (gitCache && gitCachePath === proj.path) {
    const dirty = gitCache.files?.length || 0;
    const changed = gitCache.files?.slice(0, 3).map(f => f.path) || [];
    const more = dirty > 3 ? `+${dirty - 3} more files` : '';
    cards.push(
      store.h('div', { class: 'info-card' },
        store.h('div', { class: 'panel-label' }, 'GIT'),
        store.h('div', { class: 'info-row' },
          store.h('span', { class: 'k', text: 'branch' }),
          store.h('span', { class: 'v mono', text: gitCache.branch || '—' })
        ),
        store.h('div', { class: 'info-row' },
          store.h('span', { class: 'k', text: 'dirty' }),
          store.h('span', { class: 'v mono', text: String(dirty) })
        ),
        ...changed.map(path =>
          store.h('div', { class: 'info-row' },
            store.h('span', { class: 'v mono', text: path })
          )
        ),
        more && store.h('div', { class: 'info-row' },
          store.h('span', { class: 'v mono', text: more })
        ),
        store.h('div', { class: 'row' },
          store.h('button', { class: 'chip', text: 'Diff', onclick: () => store.panel('git') }),
          store.h('button', { class: 'chip', text: 'Commit', onclick: () => {
            store.panel('git');
            setTimeout(() => focusCommit(), 100);
          }})
        )
      )
    );
  } else if (gitCachePath !== proj.path) {
    // Another project: its own git data, not the last one's.
    gitCache = null;
    gitCachePath = proj.path;
    fetchGitData(proj.path);
  }

  // JOBS card
  if (jobsCache) {
    const enabled = jobsCache.jobs?.filter(j => j.enabled).length || 0;
    const next = jobsCache.jobs
      ?.filter(j => j.enabled && j.next)
      .sort((a, b) => a.next - b.next)[0];
    cards.push(store.h('div', { class: 'info-card' },
      store.h('div', { class: 'panel-label' }, 'JOBS'),
      next && store.h('div', { class: 'info-row' },
        store.h('span', { class: 'k', text: 'next' }),
        store.h('span', { class: 'v mono', text: next.name }),
        store.h('span', { class: 'v mono', text: fromNow(next.next) })
      ),
      store.h('div', { class: 'info-row' },
        store.h('span', { class: 'k', text: 'enabled' }),
        store.h('span', { class: 'v mono', text: String(enabled) })
      )
    ));
  }

  info.replaceChildren(...cards);
}

async function fetchGitData(path) {
  try {
    const data = await store.api(`/api/git?path=${encodeURIComponent(path)}`);
    gitCache = data;
    gitCachePath = path;
    renderInfo(store.state.snap);
  } catch (e) {
    gitCache = null;
  }
}

async function fetchJobsForInfo() {
  try {
    jobsCache = await store.api('/api/jobs');
    renderInfo(store.state.snap);
  } catch (e) {
    jobsCache = null;
  }
}

function formatUptime(secs) {
  // The desktop sends whole minutes.
  if (secs < 60) return '<1m';
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  const m = mins % 60;
  if (hours < 24) return `${hours}h ${m}m`;
  const days = Math.floor(hours / 24);
  const h = hours % 24;
  return `${days}d ${h}h`;
}

// ---------- files ----------

async function renderFiles(title, tools, body, proj) {
  title.textContent = 'Files';
  
  const current = dirs.get(proj.id) || proj.path;
  dirs.set(proj.id, current);

  // Add + menu in tools
  tools.replaceChildren(
    store.h('button', {
      class: 'icon',
      text: '+',
      onclick: (e) => {
        store.menu('New', [
          { label: 'File', run: () => createFile(current, proj) },
          { label: 'Folder', run: () => createFolder(current, proj) },
          '-',
          { label: 'Reveal in file manager', run: () => store.send({ kind: 'open-file', path: current }) }
        ], e.target);
      }
    })
  );

  try {
    const data = await store.api(`/api/files?path=${encodeURIComponent(current)}`);
    body.replaceChildren(renderFileBrowser(data, proj));
  } catch (e) {
    body.replaceChildren(
      store.h('div', { class: 'row-sub bad', text: String(e.message || e) })
    );
    store.failed(e);
  }
}

async function createFile(parent, proj) {
  const name = await store.ask('File name');
  if (!name) return;
  const path = `${parent}/${name}`.replace(/\/+/g, '/');
  try {
    await store.api('/api/file', { op: 'create', path, name });
    store.toast('Created');
    refreshNow();
  } catch (e) {
    store.failed(e);
  }
}

async function createFolder(parent, proj) {
  const name = await store.ask('Folder name');
  if (!name) return;
  const path = `${parent}/${name}`.replace(/\/+/g, '/');
  try {
    await store.api('/api/file', { op: 'mkdir', path });
    store.toast('Created');
    refreshNow();
  } catch (e) {
    store.failed(e);
  }
}

async function renameFile(path, proj) {
  const oldName = path.split('/').pop();
  const name = await store.ask('Rename to', oldName);
  if (!name || name === oldName) return;
  const parent = path.substring(0, path.lastIndexOf('/'));
  const to = `${parent}/${name}`.replace(/\/+/g, '/');
  try {
    await store.api('/api/file', { op: 'rename', path, to });
    store.toast('Renamed');
    refreshNow();
  } catch (e) {
    store.failed(e);
  }
}

async function deleteFile(path, name, proj) {
  if (!await store.confirm(`Delete ${name}?`, 'Delete')) return;
  try {
    await store.api('/api/file', { op: 'delete', path });
    store.toast('Deleted');
    refreshNow();
  } catch (e) {
    store.failed(e);
  }
}

function renderFileBrowser(data, proj) {
  const crumbs = store.h('div', { class: 'crumbs' });
  const parts = data.path.split('/').filter(Boolean);
  let acc = '';
  crumbs.append(
    store.h('button', {
      class: 'chip',
      text: '/',
      onclick: () => {
        dirs.set(proj.id, '/');
        store.emit();
      }
    })
  );
  for (let i = 0; i < parts.length; i++) {
    acc += '/' + parts[i];
    const path = acc;
    crumbs.append(
      store.h('button', {
        class: 'chip',
        text: parts[i],
        onclick: () => {
          dirs.set(proj.id, path);
          store.emit();
        }
      })
    );
  }

  const rows = data.entries.map((entry) =>
    store.h(
      'div',
      { class: entry.dir ? 'tree-row dir' : 'tree-row' },
      store.h('button', {
        class: 'row-main',
        onclick: () => {
          if (entry.dir) {
            dirs.set(proj.id, entry.path);
            store.emit();
          } else {
            showFilePreview(entry.path, proj);
          }
        }
      }, entry.name),
      store.h('button', {
        class: 'row-tools',
        text: '⋮',
        onclick: (e) => {
          const rel = entry.path.startsWith(proj.path)
            ? entry.path.substring(proj.path.length).replace(/^\//, '')
            : entry.path;
          store.menu(entry.name, [
            { label: 'Open on desktop', run: () => store.send({ kind: 'open-file', path: entry.path, with: null }) },
            !entry.dir && store.shown() && {
              label: 'Attach to line',
              run: () => {
                attachFile(entry);
                store.view('stage');
              },
            },
            entry.dir && { label: 'Open in terminal', run: () => store.send({ kind: 'open-file', path: entry.path }) },
            entry.dir && { label: 'New terminal here', run: () => store.send({ kind: 'terminal-at', path: entry.path }) },
            '-',
            { label: 'Rename', run: () => renameFile(entry.path, proj) },
            { label: 'Delete', danger: true, run: () => deleteFile(entry.path, entry.name, proj) },
            '-',
            { label: 'Copy path', run: () => store.copy(entry.path) },
            { label: 'Copy relative path', run: () => store.copy(rel) }
          ], e.target);
        }
      })
    )
  );

  return store.h('div', {}, crumbs, ...rows);
}

async function showFilePreview(path, proj) {
  const body = store.el('panel-body');
  previewing = true;
  body.replaceChildren(store.h('div', { class: 'dim', text: 'Loading...' }));

  try {
    const data = await store.api(`/api/file?path=${encodeURIComponent(path)}`);
    const lines = data.text.split('\n').map(line =>
      store.h('div', { text: line || ' ' })
    );

    const tools = [
      store.h('button', {
        class: 'chip',
        text: '← Back',
        onclick: () => { previewing = false; refreshNow(); }
      })
    ];

    // Add "Open in terminal" for phone when showing a live window
    const shown = store.shown();
    if (shown) {
      const win = store.windowById(shown);
      if (win && win.alive) {
        tools.push(
          store.h('button', {
            class: 'chip',
            text: 'Open in terminal',
            onclick: () => {
              const shellQuoted = "'" + path.replace(/'/g, "'\\''") + "'";
              const wasDriving = store.driving(win.id);
              if (!wasDriving) store.send({ kind: 'takeover', window: win.id });
              store.send({ kind: 'keys', window: win.id, text: `\${EDITOR:-vi} ${shellQuoted}` });
              store.send({ kind: 'key', window: win.id, name: 'Enter' });
              if (!wasDriving) store.send({ kind: 'release', window: win.id });
              store.view('stage');
            }
          })
        );
      }
    }

    // Keep "Open on desktop"
    tools.push(
      store.h('button', {
        class: 'chip',
        text: 'Open on desktop',
        onclick: () => store.send({ kind: 'open-file', path, with: null })
      })
    );

    // Attach the whole file, or just the lines selected in it.
    if (shown) {
      tools.push(
        store.h('button', {
          class: 'chip',
          text: 'Attach',
          onpointerdown: (e) => e.preventDefault(),
          onclick: () => {
            const picked = getSelection().toString();
            if (picked.trim()) attachSnippet(picked);
            else attachFile({ path, name: path.split('/').pop() });
            store.view('stage');
          }
        })
      );
    }

    body.replaceChildren(
      ...tools,
      store.h('div', { class: 'mono dim', text: path }),
      ...(data.truncated ? [store.h('div', { class: 'row-sub bad', text: 'File truncated at 256 KiB' })] : []),
      store.h('div', { class: 'diff' }, ...lines)
    );
  } catch (e) {
    body.replaceChildren(
      store.h('button', {
        class: 'chip',
        text: '← Back',
        onclick: () => { previewing = false; refreshNow(); }
      }),
      store.h('div', { class: 'row-sub bad', text: String(e.message || e) })
    );
    store.failed(e);
  }
}

// ---------- git ----------

async function renderGit(title, tools, body, proj) {
  title.textContent = 'Source Control';
  tools.replaceChildren();

  gitSub = false;
  gitRaw = '';
  const key = `taix.look.${proj.path}`;
  try { look = JSON.parse(localStorage.getItem(key)); } catch (e) { look = null; }
  if (!look || !/^[0-9a-f]{4,64}$/i.test(look.sha || '')) look = null;
  let marked = false;

  // `force`: a user action's own change. The poll leaves a sub-view, a
  // focused commit box and an unchanged repository alone.
  const load = async (force) => {
    try {
      const data = await store.api(`/api/git?path=${encodeURIComponent(proj.path)}`);
      const raw = JSON.stringify(data);
      if (!force && (gitSub || raw === gitRaw || document.activeElement === commitInputRef)) return;
      gitRaw = raw;
      gitCache = data;
      gitCachePath = proj.path;
      // Mark this visit; the next one reviews everything after it.
      if (!marked && data.log.length) {
        marked = true;
        try { localStorage.setItem(key, JSON.stringify({ sha: data.log[0].hash, at: Math.floor(Date.now() / 1000) })); } catch (e) { /* private mode */ }
      }
      body.replaceChildren(renderGitView(data, proj));
      fetchPr(proj, data.branch);
    } catch (e) {
      body.replaceChildren(
        store.h('div', { class: 'row-sub bad', text: String(e.message || e) })
      );
      store.failed(e);
    }
  };
  gitReload = () => { gitSub = false; return load(true); };

  await load(true);

  // Poll every 5s while visible
  gitTimer = setInterval(() => {
    if (document.visibilityState === 'visible' && store.state.panel === 'git') {
      load(false);
    }
  }, 5000);
}

function renderGitView(data, proj) {
  const sections = [];

  sections.push(prBox);
  if (look && (look.sha !== data.log[0]?.hash || data.files.length)) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' },
          store.h('span', { class: 'dim', text: `Last looked ${store.ago(look.at)}` }),
          store.h('span', { class: 'grow' }),
          store.h('button', { class: 'chip', text: 'Since last look', onclick: () => showSince(proj, look) })
        )
      )
    );
  }

  // Worktree actions (when this window has its own branch)
  const win = store.windowById(store.state.shown);
  if (win && win.branch) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' },
          store.h('span', { class: 'mono', text: win.branch }),
          store.h('span', { class: 'grow' }),
          store.h('button', { class: 'chip', text: 'Diff vs base', onclick: () => showWorktreeDiff(proj) }),
          store.h('button', { class: 'chip', text: 'Merge into base', onclick: async () => {
            if (await store.confirm('Merge this worktree back into its base branch?', 'Merge')) {
              store.send({ kind: 'merge', window: win.id });
            }
          }})
        )
      )
    );
  }

  // Branch line
  const branchTools = [
    store.h('button', { class: 'chip', text: 'Fetch', onclick: () => gitOp(proj, { op: 'fetch', prune: false }) }),
    store.h('button', { class: 'chip', text: 'Pull', onclick: () => gitOp(proj, { op: 'pull' }) }),
    store.h('button', { class: 'chip', text: 'Push', onclick: () => gitOp(proj, { op: 'push', set_upstream: false, force: false }) })
  ];

  // Sequencer
  if (data.op) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' },
          store.h('span', { class: 'badge working', text: data.op }),
          store.h('span', { class: 'grow' }),
          store.h('button', { class: 'chip', text: 'Continue', onclick: () => gitOp(proj, { op: 'step', step: 'continue' }) }),
          store.h('button', { class: 'chip', text: 'Skip', onclick: () => gitOp(proj, { op: 'step', step: 'skip' }) }),
          store.h('button', { class: 'chip', text: 'Abort', onclick: () => gitOp(proj, { op: 'step', step: 'abort' }) })
        )
      )
    );
  }

  // Conflicts
  const conflicts = data.files.filter(f => f.conflict);
  if (conflicts.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' }, store.h('b', { text: 'Merge conflicts' })),
        ...conflicts.map(f =>
          store.h('div', { class: 'row' },
            store.h('span', { class: 'mono', text: f.path }),
            store.h('span', { class: 'grow' }),
            store.h('button', { class: 'chip', text: 'Ours', onclick: () => gitOp(proj, { op: 'resolve', file: f.path, side: 'ours' }) }),
            store.h('button', { class: 'chip', text: 'Theirs', onclick: () => gitOp(proj, { op: 'resolve', file: f.path, side: 'theirs' }) })
          )
        )
      )
    );
  }

  // Staged
  const staged = data.files.filter(f => f.staged);
  if (staged.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' },
          store.h('b', { text: 'Staged' }),
          store.h('span', { class: 'grow' }),
          store.h('button', { class: 'chip', text: 'Unstage all', onclick: () => gitOp(proj, { op: 'unstage-all' }) })
        ),
        ...staged.map(f => renderGitFile(f, proj, data))
      )
    );
  }

  // Changed
  const changed = data.files.filter(f => !f.staged && !f.conflict);
  if (changed.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' },
          store.h('b', { text: 'Changed' }),
          store.h('span', { class: 'grow' }),
          store.h('button', { class: 'chip', text: 'Stage all', onclick: () => gitOp(proj, { op: 'stage-all' }) }),
          store.h('button', { class: 'chip', text: 'Discard all', onclick: async () => {
            if (await store.confirm('Discard all changes?', 'Discard')) {
              gitOp(proj, { op: 'discard-all', untracked: true });
            }
          }})
        ),
        ...changed.map(f => renderGitFile(f, proj, data))
      )
    );
  }

  // Commit box
  if (data.files.some(f => f.staged)) {
    commitInputRef = store.h('input', {
      type: 'text', placeholder: 'Commit message', autocomplete: 'off', value: commitMsg,
      oninput: (e) => { commitMsg = e.target.value; }
    });
    sections.push(
      store.h('div', { class: 'panel-section' },
        commitInputRef,
        store.h('div', { class: 'row' },
          store.h('button', { class: 'chip accent', text: 'Commit', onclick: async () => {
            if (commitInputRef.value.trim() && await gitOp(proj, { op: 'commit', message: commitInputRef.value, amend: false })) commitMsg = '';
          }}),
          store.h('button', { class: 'chip', text: 'Amend', onclick: async () => {
            if (commitInputRef.value.trim() && await gitOp(proj, { op: 'commit', message: commitInputRef.value, amend: true })) commitMsg = '';
          }})
        )
      )
    );
  } else {
    commitInputRef = null;
  }

  // Branches
  if (data.branches.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' }, store.h('b', { text: 'Branches' })),
        ...data.branches.map(b =>
          store.h('div', { class: b.head ? 'row active' : 'row' },
            store.h('button', {
              class: 'row-main mono',
              text: b.name,
              onclick: () => {
                if (!b.head) gitOp(proj, { op: 'checkout', branch: b.name, create: false });
              }
            }),
            store.h('button', {
              class: 'row-tools',
              text: '⋮',
              onclick: (e) => {
                store.menu(b.name, [
                  !b.head && { label: 'Checkout', run: () => gitOp(proj, { op: 'checkout', branch: b.name, create: false }) },
                  { label: 'Rename', run: async () => {
                    const name = await store.ask('New branch name', b.name);
                    if (name) gitOp(proj, { op: 'rename-branch', from: b.name, to: name });
                  }},
                  !b.head && { label: 'Merge into current', run: () => gitOp(proj, { op: 'merge', branch: b.name, no_ff: false }) },
                  !b.head && { label: 'Rebase onto', run: () => gitOp(proj, { op: 'rebase', onto: b.name }) },
                  !b.head && { label: 'Delete', danger: true, run: () => gitOp(proj, { op: 'delete-branch', branch: b.name, force: false }) },
                  !b.head && { label: 'Delete (force)', danger: true, run: async () => {
                    if (await store.confirm(`Force-delete ${b.name}?`, 'Delete')) {
                      gitOp(proj, { op: 'delete-branch', branch: b.name, force: true });
                    }
                  }}
                ], e.target);
              }
            })
          )
        ),
        store.h('button', { class: 'chip', text: 'New branch', onclick: async () => {
          const name = await store.ask('Branch name');
          if (name) gitOp(proj, { op: 'checkout', branch: name, create: true });
        }})
      )
    );
  }

  // History
  if (data.log.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' }, store.h('b', { text: 'History' })),
        ...data.log.map(c =>
          store.h('div', { class: 'row' },
            store.h('button', {
              class: 'row-main',
              onclick: () => showCommitDiff(c.hash, proj)
            },
              store.h('div', { class: 'mono', text: c.short }),
              store.h('div', { text: c.subject }),
              store.h('div', { class: 'dim', text: `${c.author} · ${store.ago(c.when)}` })
            ),
            store.h('button', {
              class: 'row-tools',
              text: '⋮',
              onclick: (e) => {
                store.menu(c.short, [
                  { label: 'Cherry-pick', run: () => gitOp(proj, { op: 'cherry-pick', hash: c.hash }) },
                  { label: 'Revert', run: () => gitOp(proj, { op: 'revert', hash: c.hash }) },
                  { label: 'Reset --soft', run: () => gitOp(proj, { op: 'reset', hash: c.hash, mode: 'soft' }) },
                  { label: 'Reset --mixed', run: () => gitOp(proj, { op: 'reset', hash: c.hash, mode: 'mixed' }) },
                  { label: 'Reset --hard', danger: true, run: async () => {
                    if (await store.confirm(`Hard reset to ${c.short}?`, 'Reset')) {
                      gitOp(proj, { op: 'reset', hash: c.hash, mode: 'hard' });
                    }
                  }}
                ], e.target);
              }
            })
          )
        )
      )
    );
  }

  // Stashes
  if (data.stashes.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' }, store.h('b', { text: 'Stashes' })),
        ...data.stashes.map(s =>
          store.h('div', { class: 'row' },
            store.h('span', { class: 'mono', text: s.note || `stash@{${s.index}}` }),
            store.h('span', { class: 'grow' }),
            store.h('button', { class: 'chip', text: 'Apply', onclick: () => gitOp(proj, { op: 'stash-apply', index: s.index }) }),
            store.h('button', { class: 'chip', text: 'Pop', onclick: () => gitOp(proj, { op: 'stash-pop', index: s.index }) }),
            store.h('button', { class: 'chip', text: 'Drop', onclick: () => gitOp(proj, { op: 'stash-drop', index: s.index }) })
          )
        ),
        store.h('button', { class: 'chip', text: 'Stash changes', onclick: async () => {
          const msg = await store.ask('Stash message (optional)');
          if (msg !== null) gitOp(proj, { op: 'stash-push', message: msg });
        }})
      )
    );
  }

  return store.h('div', {}, ...sections);
}

function renderGitFile(f, proj, data) {
  const tools = [];
  if (f.staged) {
    tools.push(store.h('button', { class: 'chip', text: 'Unstage', onclick: () => gitOp(proj, { op: 'unstage', files: [f.path] }) }));
  } else {
    tools.push(store.h('button', { class: 'chip', text: 'Stage', onclick: () => gitOp(proj, { op: 'stage', files: [f.path] }) }));
    tools.push(store.h('button', { class: 'chip', text: 'Discard', onclick: async () => {
      if (await store.confirm(`Discard changes to ${f.path}?`, 'Discard')) {
        gitOp(proj, { op: 'discard', files: [f.path] });
      }
    }}));
  }

  return store.h('div', { class: 'row' },
    store.h('button', {
      class: 'row-main mono',
      text: f.path,
      onclick: () => showFileDiff(data.root, f.path, f.staged)
    },
      store.h('span', { class: 'badge', text: f.label })
    ),
    ...tools
  );
}

function diffLines(text) {
  return text.split('\n').map(line => {
    const cls = line.startsWith('+') ? 'add' : line.startsWith('-') ? 'del' : line.startsWith('@@') ? 'hunk' : '';
    return store.h('div', { class: cls, text: line || ' ' });
  });
}

/** A sub-view of the git panel: the poll leaves it alone until Back. */
function subView(...kids) {
  gitSub = true;
  const back = store.h('button', { class: 'chip', text: '← Back', onclick: () => gitReload && gitReload() });
  store.el('panel-body').replaceChildren(back, ...kids);
  return (...more) => store.el('panel-body').replaceChildren(back, ...more);
}

async function showDiff(url, head, text = (d) => d.text, note = () => null) {
  const set = subView(store.h('div', { class: 'dim', text: 'Loading diff...' }));
  try {
    const data = await store.api(url);
    set(
      store.h('div', { class: 'mono dim', text: head(data) }),
      note(data),
      store.h('div', { class: 'diff' }, ...diffLines(text(data)))
    );
  } catch (e) {
    set(store.h('div', { class: 'row-sub bad', text: String(e.message || e) }));
    store.failed(e);
  }
}

function showFileDiff(root, file, staged) {
  return showDiff(`/api/diff?path=${encodeURIComponent(root)}&file=${encodeURIComponent(file)}&staged=${staged ? 1 : 0}`, () => file);
}

function showCommitDiff(hash, proj) {
  return showDiff(`/api/diff?path=${encodeURIComponent(proj.path)}&commit=${encodeURIComponent(hash)}`, () => hash);
}

function showWorktreeDiff(proj) {
  return showDiff(
    `/api/worktree?path=${encodeURIComponent(proj.path)}`,
    (d) => d.base || 'base',
    (d) => d.diff,
    (d) => d.truncated && store.h('div', { class: 'dim', text: '(diff truncated)' })
  );
}

/** Commits since the mark left at the previous visit, plus what is uncommitted now. */
async function showSince(proj, mark) {
  const set = subView(store.h('div', { class: 'dim', text: 'Loading...' }));
  try {
    const d = await store.api(`/api/since?path=${encodeURIComponent(proj.path)}&mark=${mark.sha}`);
    const commits = d.commits === null
      ? [store.h('div', { class: 'dim', text: 'That commit is no longer in this history.' })]
      : d.commits.length
        ? d.commits.map(c => store.h('div', { class: 'row' },
            store.h('button', { class: 'row-main', onclick: () => showCommitDiff(c.hash, proj) },
              store.h('div', { class: 'mono', text: c.short }),
              store.h('div', { text: c.subject }),
              store.h('div', { class: 'dim', text: `${c.author} · ${store.ago(c.when)}` })
            )
          ))
        : [store.h('div', { class: 'dim', text: 'No new commits.' })];
    set(
      store.h('div', { class: 'mono dim', text: `since ${mark.sha.slice(0, 7)} · ${store.ago(mark.at)}` }),
      store.h('div', { class: 'panel-section' }, store.h('b', { text: 'Commits' }), ...commits),
      store.h('div', { class: 'panel-section' },
        store.h('b', { text: 'Uncommitted' }),
        d.truncated && store.h('div', { class: 'dim', text: '(diff truncated)' }),
        d.diff
          ? store.h('div', { class: 'diff' }, ...diffLines(d.diff))
          : store.h('div', { class: 'dim', text: 'Working tree clean.' })
      )
    );
  } catch (e) {
    set(store.h('div', { class: 'row-sub bad', text: String(e.message || e) }));
    store.failed(e);
  }
}

/** The branch's pull request, filled in when `gh` knows one; hidden otherwise. */
async function fetchPr(proj, branch) {
  let pr = {};
  try {
    pr = await store.api(`/api/pr?path=${encodeURIComponent(proj.path)}&branch=${encodeURIComponent(branch || '')}`);
  } catch (e) { /* no line is the answer */ }
  prBox.replaceChildren();
  prBox.style.display = pr.number ? '' : 'none';
  if (!pr.number) return;
  const c = pr.checks;
  const [cls, sum] = c.fail ? ['failed', `${c.fail} failing`]
    : c.pending ? ['working', `${c.pending} pending`]
    : c.pass ? ['done', `${c.pass} passing`] : ['', 'no checks'];
  prBox.append(store.h('div', { class: 'row' },
    /^https:\/\//.test(pr.url)
      ? store.h('a', { class: 'mono', href: pr.url, target: '_blank', rel: 'noopener', text: `PR #${pr.number}` })
      : store.h('span', { class: 'mono', text: `PR #${pr.number}` }),
    store.h('span', { class: 'badge', text: String(pr.state || '').toLowerCase() }),
    store.h('span', { class: `badge ${cls}`, text: sum }),
    store.h('span', { class: 'grow dim', text: pr.title })
  ));
}

async function gitOp(proj, body) {
  try {
    const result = await store.api('/api/git', { path: proj.path, ...body });
    if (result.out) store.toast(result.out);
    store.emit(); // Trigger refresh
    if (gitReload) gitReload();
    return true;
  } catch (e) {
    store.failed(e);
    return false;
  }
}

export function focusCommit() {
  if (commitInputRef) {
    commitInputRef.focus();
    commitInputRef.select();
  }
}

// ---------- jobs ----------

async function renderJobs(title, tools, body) {
  title.textContent = 'Automation';
  
  // Add + button for new job
  tools.replaceChildren(
    store.h('button', {
      class: 'icon',
      text: '+',
      onclick: () => showJobForm(null)
    })
  );

  try {
    const data = await store.api('/api/jobs');
    body.replaceChildren(renderJobsView(data));
  } catch (e) {
    body.replaceChildren(
      store.h('div', { class: 'row-sub bad', text: String(e.message || e) })
    );
    store.failed(e);
  }
}

function renderJobsView(data) {
  const sections = [];

  // Background status
  let statusText = '';
  if (data.background === 'enabled') statusText = 'Systemd timer running — jobs run even when TaiX is closed';
  else if (data.background === 'disabled') statusText = 'Systemd timer off — jobs only run while TaiX is open';
  else statusText = 'Systemd unavailable — jobs only run while TaiX is open';

  sections.push(
    store.h('div', { class: 'panel-section' },
      store.h('div', { class: 'row' },
        store.h('span', { class: 'badge', text: data.background }),
        store.h('span', { class: 'dim', text: statusText })
      )
    )
  );

  // Jobs
  if (data.jobs.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        ...data.jobs.map(job => renderJob(job))
      )
    );
  }

  // History
  if (data.runs && data.runs.length) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'row' }, store.h('b', { text: 'History' })),
        ...data.runs.slice(0, 10).map(run => renderJobRun(run))
      )
    );
  }

  return store.h('div', {}, ...sections);
}

function renderJob(job) {
  const lastBadge = job.last
    ? store.h('span', {
        class: `badge ${job.failing ? 'failed' : 'done'}`,
        text: job.last.exit !== null ? `exit ${job.last.exit}` : job.last.error || 'done'
      })
    : null;

  const nextText = job.next ? fromNow(job.next)
    : job.schedule.startsWith('on ') ? (job.enabled ? 'when it happens' : '—')
    : job.enabled ? 'pending' : '—';

  return store.h('div', { class: job.enabled ? 'job-row' : 'job-row off' },
    store.h('div', { class: 'row' },
      store.h('div', { class: 'row-main' },
        store.h('div', { text: job.name }),
        store.h('div', { class: 'mono dim', text: `${job.schedule} · ${job.action}` }),
        store.h('div', { class: 'dim', text: `Next: ${nextText}` }),
        lastBadge && store.h('div', {}, lastBadge)
      ),
      store.h('button', {
        class: 'row-tools',
        text: '⋮',
        onclick: (e) => {
          store.menu(job.name, [
            { label: 'Edit', run: () => showJobForm(job) },
            { label: 'Run now', run: () => jobOp({ op: 'run', id: job.id }) },
            job.enabled
              ? { label: 'Disable', run: () => jobOp({ op: 'disable', id: job.id }) }
              : { label: 'Enable', run: () => jobOp({ op: 'enable', id: job.id }) },
            { label: 'Log', run: () => showJobLog(job) },
            '-',
            { label: 'Delete', danger: true, run: async () => {
              if (await store.confirm(`Delete job "${job.name}"?`, 'Delete')) {
                jobOp({ op: 'delete', id: job.id });
              }
            }}
          ], e.target);
        }
      })
    )
  );
}

function renderJobRun(run) {
  const badge = run.ok
    ? store.h('span', { class: 'badge done', text: 'ok' })
    : store.h('span', { class: 'badge failed', text: 'failed' });

  return store.h('div', {
    class: 'row',
    onclick: () => showJobLog({ id: run.job, name: run.name }, run.started)
  },
    store.h('div', { class: 'row-main' },
      store.h('div', { class: 'mono', text: run.name || run.job }),
      store.h('div', { class: 'dim', text: `${store.ago(run.started)} · ${run.summary || '—'}` })
    ),
    badge
  );
}

function showJobForm(job) {
  const isEdit = !!job;
  const modal = store.el('modal');
  const card = store.el('modal-card');

  // Form state
  const state = job ? {
    id: job.id,
    name: job.name,
    schedule: job.schedule,
    action: job.action,
    command: job.command || '',
    project: job.project || '',
    harness: job.harness || '',
    notify: job.notify || 'never',
    enabled: job.enabled !== false
  } : {
    name: '',
    schedule: '@hourly',
    action: 'shell',
    command: '',
    project: '',
    harness: '',
    notify: 'never',
    enabled: true
  };

  const nameInput = store.h('input', { type: 'text', value: state.name, placeholder: 'Job name' });
  const presets = ['@hourly', '@daily', '@every 15m', 'on agent-waiting', 'on agent-idle'];
  const scheduleSelect = store.h('select', {},
    store.h('option', { value: '@hourly', text: 'Hourly', selected: state.schedule === '@hourly' }),
    store.h('option', { value: '@daily', text: 'Daily', selected: state.schedule === '@daily' }),
    store.h('option', { value: '@every 15m', text: 'Every 15 minutes', selected: state.schedule === '@every 15m' }),
    store.h('option', { value: 'on agent-waiting', text: 'When an agent is waiting', selected: state.schedule === 'on agent-waiting' }),
    store.h('option', { value: 'on agent-idle', text: 'When an agent goes idle', selected: state.schedule === 'on agent-idle' }),
    store.h('option', { value: 'custom', text: 'Custom cron or event...', selected: !presets.includes(state.schedule) })
  );
  const scheduleCustom = store.h('input', {
    type: 'text',
    value: !presets.includes(state.schedule) ? state.schedule : '',
    placeholder: '*/5 * * * *  or  on port-up:3000  or  on job-done:1',
    style: presets.includes(state.schedule) ? 'display:none' : ''
  });
  scheduleSelect.onchange = () => {
    scheduleCustom.style.display = scheduleSelect.value === 'custom' ? '' : 'none';
  };

  const actionSelect = store.h('select', {},
    store.h('option', { value: 'shell', text: 'Shell', selected: state.action === 'shell' }),
    store.h('option', { value: 'agent', text: 'Agent', selected: state.action === 'agent' }),
    store.h('option', { value: 'session', text: 'Session', selected: state.action === 'session' }),
    store.h('option', { value: 'git-sync', text: 'Git sync', selected: state.action === 'git-sync' })
  );

  const commandInput = store.h('input', { type: 'text', value: state.command, placeholder: 'Command or prompt' });
  
  const projects = store.projects();
  const projectSelect = store.h('select', {},
    store.h('option', { value: '', text: '(none)' }),
    ...projects.map(p =>
      store.h('option', { value: String(p.id), text: p.name, selected: String(state.project) === String(p.id) })
    )
  );

  const harnessInput = store.h('input', { type: 'text', value: state.harness, placeholder: 'Harness name (optional)' });

  const notifySelect = store.h('select', {},
    store.h('option', { value: 'never', text: 'Never', selected: state.notify === 'never' }),
    store.h('option', { value: 'failure', text: 'On failure', selected: state.notify === 'failure' }),
    store.h('option', { value: 'always', text: 'Always', selected: state.notify === 'always' })
  );

  const enabledSwitch = store.h('input', { type: 'checkbox', checked: state.enabled });

  const save = async () => {
    const finalSchedule = scheduleSelect.value === 'custom' ? scheduleCustom.value.trim() : scheduleSelect.value;
    const payload = {
      op: isEdit ? 'update' : 'create',
      name: nameInput.value.trim(),
      schedule: finalSchedule,
      action: actionSelect.value,
      command: commandInput.value.trim() || undefined,
      project: projectSelect.value || undefined,
      harness: harnessInput.value.trim() || undefined,
      notify: notifySelect.value,
      enabled: enabledSwitch.checked
    };
    if (isEdit) payload.id = state.id;

    try {
      await store.api('/api/jobs', payload);
      modal.hidden = true;
      store.toast(isEdit ? 'Job updated' : 'Job created');
      store.emit();
    } catch (e) {
      store.failed(e);
    }
  };

  card.replaceChildren(
    store.h('header', { class: 'mono', text: isEdit ? 'Edit job' : 'New job' }),
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Name' }),
      nameInput
    ),
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Schedule' }),
      scheduleSelect,
      scheduleCustom
    ),
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Action' }),
      actionSelect
    ),
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Command' }),
      commandInput
    ),
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Project' }),
      projectSelect
    ),
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Harness' }),
      harnessInput
    ),
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Notify' }),
      notifySelect
    ),
    store.h('div', { class: 'field switch' },
      store.h('label', { text: 'Enabled' }),
      enabledSwitch
    ),
    store.h('footer', {},
      store.h('button', { text: 'Cancel', onclick: () => { modal.hidden = true; } }),
      store.h('button', { class: 'accent', text: isEdit ? 'Save' : 'Create', onclick: save })
    )
  );

  modal.hidden = false;
  nameInput.focus();
}

async function showJobLog(job, at) {
  const body = store.el('panel-body');
  body.replaceChildren(store.h('div', { class: 'dim', text: 'Loading log...' }));

  try {
    const data = await store.api(`/api/joblog?id=${job.id}${at ? `&at=${at}` : ''}`);
    const lines = data.text.split('\n').map(line =>
      store.h('div', { text: line || ' ' })
    );
    body.replaceChildren(
      store.h('button', {
        class: 'chip',
        text: '← Back',
        onclick: () => store.emit()
      }),
      store.h('button', {
        class: 'chip',
        text: 'Run again',
        onclick: async () => {
          await jobOp({ op: 'run', id: job.id });
          store.toast('Running…');
        }
      }),
      store.h('div', { class: 'mono dim', text: at ? `${job.name} · ${store.ago(at)}` : job.name }),
      store.h('div', { class: 'diff' }, ...lines)
    );
  } catch (e) {
    body.replaceChildren(
      store.h('button', {
        class: 'chip',
        text: '← Back',
        onclick: () => store.emit()
      }),
      store.h('div', { class: 'row-sub bad', text: String(e.message || e) })
    );
    store.failed(e);
  }
}

async function jobOp(body) {
  try {
    await store.api('/api/jobs', body);
    refreshNow();
  } catch (e) {
    store.failed(e);
  }
}

function fromNow(unix) {
  if (!unix) return 'never';
  const secs = Math.max(0, Math.floor(unix - Date.now() / 1000));
  if (secs < 60) return `in ${secs}s`;
  if (secs < 3600) return `in ${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `in ${Math.floor(secs / 3600)}h`;
  return `in ${Math.floor(secs / 86400)}d`;
}

// ---------- settings ----------

async function renderSettings(title, tools, body) {
  title.textContent = 'Settings';
  tools.replaceChildren();

  const sections = [];
  const snap = store.state.snap;

  // Fetch server config
  let config = null;
  try {
    config = await store.api('/api/config');
  } catch (e) {
    // Config endpoint not available
  }

  // Display section
  const fontScale = parseFloat(localStorage['taix-font-scale'] || '1.0');
  const displayRows = [
    store.h('div', { class: 'field' },
      store.h('label', { text: 'Font scale (local)' }),
      store.h('input', {
        type: 'range',
        min: '0.7',
        max: '1.6',
        step: '0.05',
        value: String(fontScale),
        oninput: (e) => {
          localStorage['taix-font-scale'] = e.target.value;
          window.dispatchEvent(new CustomEvent('taix-font-scale'));
        }
      }),
      store.h('span', { class: 'mono', text: fontScale.toFixed(2) })
    )
  ];

  if (config) {
    const themeSelect = store.h('select', {},
      ['taix', 'catppuccin-mocha', 'catppuccin-latte', 'pywal'].map(t =>
        store.h('option', { value: t, text: t, selected: (config.theme || 'taix') === t })
      )
    );
    themeSelect.onchange = () => applyConfig({ theme: themeSelect.value });

    const fontSizeInput = store.h('input', {
      type: 'number',
      min: '6',
      max: '24',
      value: String(config.font_size || 13)
    });
    fontSizeInput.onchange = () => applyConfig({ font_size: parseInt(fontSizeInput.value, 10) });

    displayRows.push(
      store.h('div', { class: 'field' },
        store.h('label', { text: 'Theme' }),
        themeSelect
      ),
      store.h('div', { class: 'field' },
        store.h('label', { text: 'Pane font size' }),
        fontSizeInput
      )
    );
  }

  sections.push(
    store.h('div', { class: 'panel-section' },
      store.h('div', { class: 'panel-label' }, 'Display'),
      ...displayRows
    )
  );

  // Keyboard section
  const mine = myKeys();
  const symEnabled = localStorage['taix-keys-sym'] !== 'off';
  const ctlEnabled = localStorage['taix-keys-ctl'] !== 'off';
  sections.push(
    store.h('div', { class: 'panel-section' },
      store.h('div', { class: 'panel-label' }, 'Keyboard'),
      store.h('div', { class: 'field switch' },
        store.h('label', { text: 'Symbol keys' }),
        store.h('input', {
          type: 'checkbox',
          checked: symEnabled,
          onchange: (e) => {
            if (e.target.checked) {
              delete localStorage['taix-keys-sym'];
              document.documentElement.removeAttribute('data-keys-sym');
            } else {
              localStorage['taix-keys-sym'] = 'off';
              document.documentElement.setAttribute('data-keys-sym', 'off');
            }
          }
        })
      ),
      store.h('div', { class: 'field switch' },
        store.h('label', { text: 'Control keys' }),
        store.h('input', {
          type: 'checkbox',
          checked: ctlEnabled,
          onchange: (e) => {
            if (e.target.checked) {
              delete localStorage['taix-keys-ctl'];
              document.documentElement.removeAttribute('data-keys-ctl');
            } else {
              localStorage['taix-keys-ctl'] = 'off';
              document.documentElement.setAttribute('data-keys-ctl', 'off');
            }
          }
        })
      ),
      store.h('div', { class: 'field' },
        store.h('label', { text: mine.length ? 'Custom keys (hold a key to add)' : 'Custom keys: hold a key, or tap +' }),
        mine.length ? store.h('button', {
          class: 'chip',
          text: 'Reset',
          onclick: () => { saveMyKeys([]); store.emit(); }
        }) : null
      ),
      ...mine.map((k, i) =>
        store.h('div', { class: 'field' },
          store.h('label', { class: 'mono', text: `${k.label}  ${k.kind === 'run' ? `${k.value} ⏎` : k.value}` }),
          store.h('button', {
            class: 'chip',
            text: 'Delete',
            onclick: () => { saveMyKeys(myKeys().filter((_, j) => j !== i)); store.emit(); }
          })
        )
      )
    )
  );

  // Notifications section (only if supported)
  if (store.notify.supported()) {
    const notifyEnabled = store.notify.enabled();
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'panel-label' }, 'Notifications'),
        store.h('div', { class: 'field switch' },
          store.h('label', { text: 'Browser notifications' }),
          store.h('input', {
            type: 'checkbox',
            checked: notifyEnabled,
            onchange: async (e) => {
              if (e.target.checked) {
                const granted = await store.notify.enable();
                if (!granted) {
                  e.target.checked = false;
                  store.toast('Notification permission denied', 'bad');
                }
              } else {
                store.notify.disable();
              }
            }
          })
        )
      )
    );
  }

  // Lock-screen alerts. iOS Safari only has Web Push inside the Home Screen
  // app, so in a plain tab there is nothing to switch on, only what to do.
  if (store.push.needsInstall()) {
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'panel-label' }, 'Lock-screen alerts'),
        store.h('div', { class: 'row-sub', text: 'Add TaiX to the Home Screen first (Share, then Add to Home Screen), then open it from there.' })
      )
    );
  } else if (store.push.supported()) {
    const alerts = store.h('input', {
      type: 'checkbox',
      onchange: async (e) => {
        const on = e.target.checked;
        try {
          if (on && !(await store.push.enable())) {
            e.target.checked = false;
            store.toast('Notification permission denied', 'bad');
          } else if (!on) {
            await store.push.disable();
          }
        } catch (err) {
          e.target.checked = !on;
          store.failed(err);
        }
      }
    });
    // Asked after the section is up: `ready` waits on the worker, and the
    // rest of Settings must not.
    store.push.current().then((sub) => { alerts.checked = !!sub; });
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'panel-label' }, 'Lock-screen alerts'),
        store.h('div', { class: 'field switch' },
          store.h('label', { text: 'Notify me when an agent waits' }),
          alerts
        )
      )
    );
  }

  // Face ID: only where WebAuthn exists (localhost, or the tailnet's https).
  if (lock.supported()) {
    const faceId = store.h('input', {
      type: 'checkbox',
      onchange: async (e) => {
        const on = e.target.checked;
        try {
          if (on) await lock.register();
          else await lock.remove();
          store.toast(on ? 'Face ID is now required on this device' : 'Face ID removed');
        } catch (err) {
          e.target.checked = !on;
          store.failed(err);
        }
      }
    });
    lock.state().then((s) => { faceId.checked = s.registered; }, () => {});
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'panel-label' }, 'Security'),
        store.h('div', { class: 'field switch' },
          store.h('label', { text: 'Require Face ID on this device' }),
          faceId
        )
      )
    );
  }

  // Server settings
  if (config) {
    const idleInput = store.h('input', {
      type: 'number',
      min: '0',
      step: '1000',
      value: String(config.idle_after_ms || 0)
    });
    idleInput.onchange = () => applyConfig({ idle_after_ms: parseInt(idleInput.value, 10) });

    const isolateSwitch = store.h('input', { type: 'checkbox', checked: config.isolate || false });
    isolateSwitch.onchange = () => applyConfig({ isolate: isolateSwitch.checked });

    const editorInput = store.h('input', { type: 'text', value: config.editor || '' });
    editorInput.onchange = () => applyConfig({ editor: editorInput.value.trim() });

    const reapInput = store.h('input', {
      type: 'number',
      min: '0',
      step: '1000',
      value: String(config.reap_idle_after_ms || 0)
    });
    reapInput.onchange = () => applyConfig({ reap_idle_after_ms: parseInt(reapInput.value, 10) });

    const portInput = store.h('input', { type: 'number', min: '1024', max: '65535', value: String(config.web_port || 7777) });
    portInput.onchange = () => applyConfig({ web_port: parseInt(portInput.value, 10) });

    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'panel-label' }, 'Server'),
        store.h('div', { class: 'field' },
          store.h('label', { text: 'Idle timeout (ms)' }),
          idleInput
        ),
        store.h('div', { class: 'field switch' },
          store.h('label', { text: 'Isolate worktrees' }),
          isolateSwitch
        ),
        store.h('div', { class: 'field' },
          store.h('label', { text: 'Editor' }),
          editorInput
        ),
        store.h('div', { class: 'field' },
          store.h('label', { text: 'Reap idle after (ms, 0=never)' }),
          reapInput
        ),
        store.h('div', { class: 'field' },
          store.h('label', { text: 'Web port' }),
          portInput
        )
      )
    );
  }

  sections.push(settingsSection());

  // Server info
  if (snap?.health) {
    const health = snap.health;
    sections.push(
      store.h('div', { class: 'panel-section' },
        store.h('div', { class: 'panel-label' }, 'Pairing'),
        store.h('div', { class: 'info-row' },
          store.h('span', { class: 'k', text: 'link' }),
          store.h('span', { class: 'v mono', text: health.url || '—' })
        ),
        store.h('div', { class: 'row' },
          store.h('button', {
            class: 'chip',
            text: 'Copy',
            onclick: () => store.copy(health.url || '', 'Pairing link copied')
          })
        )
      )
    );
  }
  sections.push(hostList.section());

  body.replaceChildren(store.h('div', {}, ...sections));
}

async function applyConfig(patch) {
  try {
    await store.send({ kind: 'config', patch });
    store.toast('Config updated');
    // Also call the API to persist it
    await store.api('/api/config', patch);
  } catch (e) {
    store.failed(e);
  }
}
