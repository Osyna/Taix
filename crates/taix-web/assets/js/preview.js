// The ports panel, and the frame that previews one of them.
//
// A port is previewed through the server's own `/p/<port>/` proxy, so the
// page works from a phone whose loopback is not the desktop's. The frame is
// a sibling of the stage inside #body: beside the terminal where the layout
// has columns, over everything on a phone (style.css decides which).
import * as store from './store.js';

const { h } = store;
let frame = null;

function build() {
  const title = h('span', { id: 'preview-title' });
  const iframe = h('iframe', { title: 'Preview' });
  const open = h('a', { class: 'chip', text: 'Open', target: '_blank', rel: 'noopener' });
  const box = h(
    'aside',
    { id: 'preview', 'aria-label': 'Preview' },
    h(
      'header',
      { id: 'preview-top' },
      title,
      h('span', { class: 'grow' }),
      open,
      // The frame is sandboxed (opaque origin): its location is not ours to
      // read, but its src attribute is, and setting it navigates again.
      h('button', { class: 'chip', text: 'Reload', onclick: () => { iframe.src = iframe.getAttribute('src'); } }),
      h('button', { class: 'icon', title: 'Close preview', onclick: close, html: '<svg class="ico"><use href="#i-close" /></svg>' }),
    ),
    iframe,
  );
  store.el('body').insertBefore(box, store.el('panel'));
  return { box, title, iframe, open };
}

export function close() {
  if (!frame) return;
  frame.box.hidden = true;
  // A frame left running keeps its dev server's hot-reload socket open.
  frame.iframe.src = 'about:blank';
}

// Panel and preview share the stage's right side: whichever opens last wins.
store.onPanel((name) => name && close());

export function preview(port) {
  frame ??= build();
  frame.title.textContent = `PREVIEW :${port}`;
  frame.open.href = `/p/${port}/`;
  frame.iframe.src = `/p/${port}/`;
  frame.box.hidden = false;
  // The panel that launched it would squeeze terminal and page to slivers.
  if (store.state.panel) store.panel(store.state.panel);
}

/** Every port a pane process listens on, per window, with its two actions. */
export function renderPorts(title, tools, body) {
  title.textContent = 'Ports';
  tools.replaceChildren();
  const rows = [];
  for (const proj of store.projects()) {
    for (const win of proj.windows) {
      for (const port of win.ports || []) {
        rows.push(
          h(
            'div',
            { class: 'port-row' },
            h('b', { text: `:${port}` }),
            h('span', { class: 'dim', text: `${win.name} · ${proj.name}` }),
            h('span', { class: 'grow' }),
            h('button', { class: 'chip', text: 'Preview', onclick: () => preview(port) }),
            h('a', { class: 'chip', text: 'Open', href: `/p/${port}/`, target: '_blank', rel: 'noopener' }),
          ),
        );
      }
    }
  }
  body.replaceChildren(
    ...(rows.length
      ? rows
      : [h('div', { class: 'dim', text: 'No pane is serving a port yet. Start a dev server in a terminal (say python3 -m http.server 8000) and it shows up here.' })]),
  );
}
