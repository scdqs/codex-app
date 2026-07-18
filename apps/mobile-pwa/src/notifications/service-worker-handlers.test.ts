import { describe, expect, it, vi } from "vitest";
import {
  handleNotificationClick,
  handlePush,
  type ServiceWorkerEnvironment,
  type ServiceWorkerNotificationOptions,
  type WindowClientPort,
} from "./service-worker-handlers";

describe("service worker handlers", () => {
  it("posts to visible clients without showing a system notification", async () => {
    const env = harness({ visibleClients: 1 });

    await handlePush(payload(), env.environment, 10);

    expect(env.postedMessages).toHaveLength(1);
    expect(env.notifications).toHaveLength(0);
  });

  it("shows a system notification using the device sound setting when hidden", async () => {
    const env = harness({ visibleClients: 0 });

    await handlePush(payload({ kind: "error", silent: true }), env.environment, 10);

    expect(env.notifications[0]).toMatchObject({
      title: "Release",
      options: {
        body: "Codex task stopped with an error",
        tag: "alert-1",
        silent: true,
      },
    });
  });

  it("forces a system notification while a settings client is visible", async () => {
    const env = harness({ visibleClients: 1 });

    await handlePush(payload({ forceSystemNotification: true }), env.environment, 10);

    expect(env.notifications).toHaveLength(1);
    expect(env.postedMessages).toHaveLength(0);
  });

  it("focuses an existing client and requests the target thread", async () => {
    const env = harness({ allClients: 1 });

    await handleNotificationClick({ threadId: "thread-9" }, env.environment);

    expect(env.focusedClients).toEqual([0]);
    expect(env.postedMessages[0]).toEqual({ type: "open_thread", threadId: "thread-9" });
  });

  it("opens a new window with the encoded thread query when no client exists", async () => {
    const env = harness({ allClients: 0 });

    await handleNotificationClick({ threadId: "thread/9" }, env.environment);

    expect(env.openedUrls).toEqual(["/?threadId=thread%2F9"]);
  });
});

function payload(overrides: Record<string, unknown> = {}) {
  return {
    eventId: "alert-1",
    kind: "completed",
    threadId: "thread-1",
    threadTitle: "Release",
    occurredAt: 1,
    vibrationEnabled: true,
    vibrationPattern: [80],
    silent: false,
    forceSystemNotification: false,
    ...overrides,
  };
}

function harness(options: { visibleClients?: number; allClients?: number }) {
  const postedMessages: unknown[] = [];
  const notifications: Array<{ title: string; options: ServiceWorkerNotificationOptions }> = [];
  const focusedClients: number[] = [];
  const openedUrls: string[] = [];
  const clients = Array.from({ length: options.allClients ?? options.visibleClients ?? 0 }, (_, index) =>
    client(index),
  );
  function client(index: number): WindowClientPort {
    return {
      visibilityState: index < (options.visibleClients ?? 0) ? "visible" : "hidden",
      url: "https://codex.example.com/",
      postMessage: (message) => postedMessages.push(message),
      focus: vi.fn(async () => {
        focusedClients.push(index);
        return clients[index] ?? client(index);
      }),
    };
  }
  const environment: ServiceWorkerEnvironment = {
    claimEvent: vi.fn(async () => true),
    visibleWindowClients: vi.fn(async () =>
      clients.filter((candidate) => candidate.visibilityState === "visible"),
    ),
    showNotification: vi.fn(async (title, options) => {
      notifications.push({ title, options });
    }),
    openWindow: vi.fn(async (url) => {
      openedUrls.push(url);
      return null;
    }),
    allWindowClients: vi.fn(async () => clients),
  };
  return { environment, postedMessages, notifications, focusedClients, openedUrls };
}
