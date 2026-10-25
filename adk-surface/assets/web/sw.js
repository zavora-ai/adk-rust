// The phone's service worker (spec 3.3 §5.4, §11 S-I5). A wake-up carries nothing but an id:
// it shows these fixed words first, never anything from the push, then reads the badge and
// acknowledges the wake-up, best effort. The words are the engine's (`words.rs`); a test keeps
// them equal.
const TITLE = "A decision is waiting";
const BODY = "Open the app to see it";
const DATABASE = "adk-home";
const STORE = "device";
const KEY = "token";
const DECISIONS = "/ui/decisions";

/** The phone's device token, where the page keeps it; none when storage is off. */
function readToken() {
  return new Promise((resolve) => {
    try {
      const open = indexedDB.open(DATABASE, 1);
      open.onupgradeneeded = () => open.result.createObjectStore(STORE);
      open.onerror = () => resolve(undefined);
      open.onsuccess = () => {
        const db = open.result;
        try {
          const get = db.transaction(STORE, "readonly").objectStore(STORE).get(KEY);
          get.onsuccess = () => {
            db.close();
            resolve(typeof get.result === "string" && get.result ? get.result : undefined);
          };
          get.onerror = () => {
            db.close();
            resolve(undefined);
          };
        } catch {
          db.close();
          resolve(undefined);
        }
      };
    } catch {
      resolve(undefined);
    }
  });
}

/** The wake-up's id, the only thing a push carries. */
function wakeId(event) {
  try {
    const data = event.data ? event.data.json() : undefined;
    return data && typeof data.id === "string" ? data.id : undefined;
  } catch {
    return undefined;
  }
}

async function wake(event) {
  const id = wakeId(event);
  await self.registration.showNotification(TITLE, {
    body: BODY,
    tag: "adk-decision",
    icon: "/ui/icons/icon-192.png",
    badge: "/ui/icons/icon-192.png",
    data: { url: DECISIONS },
  });
  const token = await readToken();
  if (!token) return;
  const headers = {
    accept: "application/json",
    authorization: `Bearer ${token}`,
    "content-type": "application/json",
  };
  try {
    const response = await fetch("/api/surface/status?surface=phone", { headers });
    if (response.ok) {
      const status = await response.json();
      if (self.navigator && self.navigator.setAppBadge) await self.navigator.setAppBadge(status.decisions || 0);
    }
  } catch {
    // The badge waits for the app to open.
  }
  if (id) {
    try {
      await fetch("/api/surface/phone/push/received", { method: "POST", headers, body: JSON.stringify({ id }) });
    } catch {
      // A missed acknowledgement only leaves out one delivery's latency.
    }
  }
}

async function open() {
  const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  for (const client of windows) {
    if ("focus" in client) {
      if ("navigate" in client) await client.navigate(DECISIONS).catch(() => undefined);
      return client.focus();
    }
  }
  return self.clients.openWindow(DECISIONS);
}

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));
self.addEventListener("push", (event) => event.waitUntil(wake(event)));
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(open());
});
