// Keeps the page's own files so that it opens without a network and says
// so, instead of the browser's error page. The network is asked first, so
// a new version is taken as soon as it is reachable.

const CACHE = "splitlane-app-v3";
const SHELL = ["./", "app.js", "link.js", "protocol.js", "scan.js", "store.js", "style.css", "version.js", "markdown.js", "prompt.js", "manifest.webmanifest", "icon-192.png"];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(SHELL)));
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((names) => Promise.all(names.filter((n) => n !== CACHE).map((n) => caches.delete(n))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET" || new URL(request.url).origin !== location.origin) return;
  event.respondWith(
    fetch(request)
      .then((response) => {
        if (response.ok) {
          const copy = response.clone();
          caches.open(CACHE).then((cache) => cache.put(request, copy));
        }
        return response;
      })
      .catch(() => caches.match(request, { ignoreSearch: true }).then((hit) => hit ?? caches.match("./"))),
  );
});
