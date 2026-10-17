// How long a window's agent has been in its current state ("busy 4m").
//
// The desktop publishes `since` (epoch seconds of the last state change) once
// per change; the clock runs here, on a coarse timer, so nothing is
// re-published to keep a counter ticking.

import { h } from './store.js';

const LABEL = { working: 'busy' };

function span(secs) {
  const m = Math.floor(Math.max(0, secs) / 60);
  if (m < 1) return '<1m';
  if (m < 60) return `${m}m`;
  const hours = Math.floor(m / 60);
  if (hours < 24) return m % 60 ? `${hours}h ${m % 60}m` : `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

function text(state, since) {
  return `${LABEL[state] || state} ${span(Date.now() / 1000 - since)}`;
}

/** A self-updating "busy 4m" node, or null for a window with no state clock. */
export function age(win) {
  if (!win.since || !win.alive || win.kind === 'browser') return null;
  return h('span', {
    class: 'age',
    text: text(win.state, win.since),
    dataset: { state: win.state, since: win.since },
  });
}

setInterval(() => {
  for (const node of document.querySelectorAll('.age[data-since]')) {
    node.textContent = text(node.dataset.state, +node.dataset.since);
  }
}, 15000);
