import * as store from './store.js';

// The panel re-reads on a timer, not on snapshots: a command finishing does
// not change a snapshot, and the search box must not be rebuilt under a
// finger.
let timer = null;

function took(ms) {
  if (ms < 1000) return `${ms} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)} s`;
  const m = Math.floor(s / 60);
  return `${m}m ${Math.round(s - m * 60)}s`;
}

function row(c, named) {
  const win = store.windowById(c.window);
  const status = c.exit === null ? '–' : c.exit === 0 ? '✓' : `✗ ${c.exit}`;
  const meta = [named && win.name, store.ago(c.at / 1000), took(c.ms)].filter(Boolean);
  return store.h('div', { class: 'row', onclick: () => store.copy(c.text, 'Command copied') },
    store.h('div', { class: 'row-main' },
      store.h('div', { class: 'mono', text: c.text || '(unreadable)' }),
      store.h('div', { class: 'dim', text: meta.join(' · ') })),
    store.h('span', { class: `badge ${c.exit ? 'bad' : 'quiet'}`, text: status }));
}

/**
 * Commands the shown window ran, or - with text in the box - every
 * window's that match. Needs a shell that sends OSC 133 marks (README).
 */
export function renderHistory(title, tools, body) {
  title.textContent = 'History';
  tools.replaceChildren();
  const search = store.h('input', { type: 'search', placeholder: 'Search every window', autocomplete: 'off' });
  const list = store.h('div', { class: 'panel-section' });
  body.replaceChildren(store.h('div', { class: 'panel-section' }, search), list);

  let last = '';
  const load = async () => {
    const q = search.value.trim();
    const path = q ? `/api/history?q=${encodeURIComponent(q)}` : `/api/history?window=${store.shown()}`;
    try {
      const { commands } = await store.api(path);
      const json = JSON.stringify(commands);
      if (json === last) return;
      last = json;
      const known = commands.filter((c) => store.windowById(c.window));
      list.replaceChildren(...(known.length
        ? known.map((c) => row(c, !!q))
        : [store.h('div', { class: 'row-sub dim', text: q
          ? 'No command matches.'
          : 'No commands yet. This needs a shell that sends OSC 133 prompt marks; see "Shell integration" in the README.' })]));
    } catch (e) {
      store.failed(e);
    }
  };
  search.addEventListener('input', load);

  clearInterval(timer);
  timer = setInterval(() => {
    if (store.state.panel !== 'history') return clearInterval(timer);
    if (document.visibilityState === 'visible') load();
  }, 2000);
  load();
}
