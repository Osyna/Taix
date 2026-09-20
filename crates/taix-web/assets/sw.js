// Exists for one reason: showNotification() needs a registration. No fetch
// handler and no cache - a phone holding a stale app.js after an update is
// a bug nobody can diagnose from the other end of the house.
self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

self.addEventListener('notificationclick', (e) => {
  e.notification.close();
  const url = e.notification.data?.url || '/';
  e.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((tabs) => {
      const tab = tabs.find((t) => new URL(t.url).origin === self.location.origin);
      if (tab) {
        tab.navigate?.(url);
        return tab.focus();
      }
      return self.clients.openWindow(url);
    }),
  );
});
