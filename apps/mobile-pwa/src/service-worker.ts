/// <reference lib="webworker" />

import { RecentEventStore } from "./notifications/recent-event-store";
import {
  handleNotificationClick,
  handlePush,
  type ServiceWorkerEnvironment,
  type WindowClientPort,
} from "./notifications/service-worker-handlers";

const worker = globalThis as unknown as ServiceWorkerGlobalScope;
const CACHE_NAME = "codex-mobile-shell-v2";
const SHELL_ASSETS = [
  "/",
  "/manifest.webmanifest",
  "/icon.svg",
  "/icon-maskable.svg",
  "/icon-192.png",
];
const recentEvents = new RecentEventStore();

const environment: ServiceWorkerEnvironment = {
  claimEvent: (eventId, now) => recentEvents.claim(eventId, now),
  visibleWindowClients: async () =>
    (await windowClients())
      .filter((client) => client.visibilityState === "visible")
      .map(asWindowClientPort),
  showNotification: (title, options) =>
    worker.registration.showNotification(title, options as NotificationOptions),
  openWindow: async (url) => {
    const client = await worker.clients.openWindow(url);
    return client ? asWindowClientPort(client) : null;
  },
  allWindowClients: async () => (await windowClients()).map(asWindowClientPort),
};

worker.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE_NAME)
      .then((cache) => cache.addAll(SHELL_ASSETS))
      .then(() => worker.skipWaiting()),
  );
});

worker.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      const names = await caches.keys();
      await Promise.all(
        names.filter((name) => name !== CACHE_NAME).map((name) => caches.delete(name)),
      );
      await worker.clients.claim();
    })(),
  );
});

worker.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (
    url.pathname.startsWith("/api/") ||
    url.pathname === "/ws" ||
    url.pathname.startsWith("/ws/")
  ) {
    return;
  }
  if (event.request.mode === "navigate") {
    event.respondWith(
      fetch(event.request).catch(async () => (await caches.match("/")) ?? Response.error()),
    );
    return;
  }
  event.respondWith(
    caches.match(event.request).then((cached) => cached ?? fetch(event.request)),
  );
});

worker.addEventListener("push", (event) => {
  event.waitUntil(
    (async () => {
      let raw: unknown;
      try {
        raw = event.data?.json();
      } catch {
        console.warn("invalid_push_payload");
        return;
      }
      await handlePush(raw, environment);
    })(),
  );
});

worker.addEventListener("notificationclick", (event) => {
  event.notification.close();
  event.waitUntil(handleNotificationClick(event.notification.data, environment));
});

async function windowClients(): Promise<readonly WindowClient[]> {
  return worker.clients.matchAll({ type: "window", includeUncontrolled: true });
}

function asWindowClientPort(client: WindowClient): WindowClientPort {
  return {
    visibilityState: client.visibilityState,
    url: client.url,
    postMessage: (message) => client.postMessage(message),
    focus: async () => asWindowClientPort(await client.focus()),
  };
}
