// The installed app's worker: notifications, and a shell that opens with
// the desktop unreachable instead of the browser's error page.
//
// Network first, always. A cache-first shell would keep running the last
// build's app.js after an update - a bug nobody can diagnose from the other
// end of the house - so the cache is only ever the fallback, refreshed by
// every successful load. Live data never touches it: the socket, commands
// and the API are not in the list.
const CACHE = 'taix-shell';
const SHELL = /^\/(|style\.css|manifest\.webmanifest|js\/[a-z]+\.js|icons\/[\w-]+\.svg|icon[\w-]*\.(png|svg)|apple-touch-icon\.png)$/;

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

self.addEventListener('fetch', (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== 'GET' || url.origin !== self.location.origin || !SHELL.test(url.pathname)) return;
  // `/?k=...` and `/#w12` are the page too; the key is not worth storing.
  const key = url.pathname === '/' ? '/' : e.request;
  e.respondWith(
    fetch(e.request).then(
      (res) => {
        if (res.ok && res.type === 'basic' && !res.headers.has('x-taix-pair')) {
          const copy = res.clone();
          e.waitUntil(caches.open(CACHE).then((c) => c.put(key, copy)));
        }
        return res;
      },
      () => caches.match(key).then((hit) => hit || Response.error()),
    ),
  );
});

// A push from the desktop: an agent stopped to ask. Shown unconditionally -
// browsers that deliver pushes require every one to become a notification -
// and tagged by window, so a second ask from the same window replaces the
// first instead of stacking. `waiting` is set only for a question: it is the
// app badge, and it is what earns the answer buttons (browsers without
// notification actions, iOS among them, simply ignore them).
const ANSWERS = { yes: 'y', continue: 'continue' };

self.addEventListener('push', (e) => {
  const msg = e.data?.json() || {};
  const asks = typeof msg.waiting === 'number';
  if (asks) self.navigator.setAppBadge?.(msg.waiting)?.catch(() => {});
  e.waitUntil(
    self.registration.showNotification(msg.title || 'TaiX', {
      body: msg.body || '',
      tag: `w${msg.window}`,
      data: { window: msg.window, url: `/#w${msg.window}` },
      actions: asks ? [{ action: 'yes', title: 'Yes' }, { action: 'continue', title: 'Continue' }] : [],
    }),
  );
});

// Answer through POST /cmd, the way the page's own sendBeacon does: a
// same-origin fetch from the worker carries the pairing cookie. Same path as
// typed input - borrow the window, type, Enter, give it back - and each POST
// is awaited so the desktop sees them in that order.
async function answer(window, text) {
  for (const cmd of [
    { kind: 'takeover', window },
    { kind: 'type', window, text },
    { kind: 'key', window, name: 'Enter' },
    { kind: 'release', window },
  ]) {
    const res = await fetch('/cmd', { method: 'POST', body: JSON.stringify(cmd) });
    if (!res.ok) return false;
  }
  return true;
}

self.addEventListener('notificationclick', (e) => {
  e.notification.close();
  const { window, url = '/' } = e.notification.data || {};
  const text = ANSWERS[e.action];
  e.waitUntil(
    (text && window != null ? answer(window, text).catch(() => false) : Promise.resolve(false)).then((sent) => {
      // Answered from the lock screen: nothing left to open.
      if (sent) return;
      return self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((tabs) => {
        const tab = tabs.find((t) => new URL(t.url).origin === self.location.origin);
        if (tab) {
          tab.navigate?.(url);
          return tab.focus();
        }
        return self.clients.openWindow(url);
      });
    }),
  );
});
