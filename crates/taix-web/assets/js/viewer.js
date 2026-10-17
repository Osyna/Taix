// The page behind a read-only share link: one window's screen, nothing to
// press. It shares no code with the app on purpose - the server only
// answers this page's own paths, and the frames it sends are cut to the one
// window the link was made for.

const $ = (id) => document.getElementById(id);

let socket = null;
let backoff = 250;
let ends = 0;
let ended = false;
let html = null;

function finish(message) {
  ended = true;
  $('v-dot').className = 'dot';
  $('v-name').textContent = message;
  $('v-state').textContent = '';
  $('v-left').textContent = '';
  socket?.close();
}

function paint(frame) {
  ends = frame.ends * 1000;
  if (frame.name === null) {
    $('v-name').textContent = 'This window is gone';
    $('v-state').textContent = '';
    $('v-dot').className = 'dot';
  } else {
    $('v-name').textContent = frame.name;
    $('v-state').textContent = frame.state;
    $('v-dot').className = `dot ${frame.state}`;
  }
  // Only a changed screen is worth touching the DOM for.
  if (frame.html !== html) {
    const page = document.scrollingElement;
    // Follow the prompt, as a terminal does, unless the visitor scrolled up.
    const follow = html === null || page.scrollTop + page.clientHeight >= page.scrollHeight - 4;
    html = frame.html;
    $('v-screen').innerHTML = html ?? '';
    if (follow) page.scrollTop = page.scrollHeight;
  }
}

function tick() {
  if (ended || !ends) return;
  const secs = Math.round((ends - Date.now()) / 1000);
  if (secs <= 0) return finish('This link has expired.');
  $('v-left').textContent = secs >= 3600 ? `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m left` : `${Math.ceil(secs / 60)}m left`;
}

function open() {
  if (ended) return;
  const url = new URL('ws', location.href);
  url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(url);
  socket = ws;
  ws.onopen = () => { backoff = 250; };
  ws.onmessage = (ev) => paint(JSON.parse(ev.data));
  ws.onclose = (ev) => {
    if (ws !== socket || ended) return;
    // 4404: the owner revoked the link or its time ran out. Reconnecting
    // would only be refused.
    if (ev.code === 4404) return finish('This link has expired or was revoked.');
    // A revoked link refuses the upgrade instead of closing it politely.
    fetch('./', { cache: 'no-store' }).then((r) => {
      if (r.status === 404) finish('This link has expired or was revoked.');
    }, () => {});
    setTimeout(open, backoff);
    backoff = Math.min(backoff * 2, 5000);
  };
}

setInterval(tick, 10000);
open();
