import "fake-indexeddb/auto";
import { IDBFactory } from "fake-indexeddb";
import { beforeEach, describe, expect, it } from "vitest";
import { RecentEventStore } from "./recent-event-store";

describe("RecentEventStore", () => {
  beforeEach(() => {
    Object.defineProperty(globalThis, "indexedDB", {
      value: new IDBFactory(),
      configurable: true,
    });
  });

  it("claims an event once and allows it after TTL expiry", async () => {
    const store = new RecentEventStore({ ttlMs: 1000, maxEntries: 2 });

    expect(await store.claim("event-1", 100)).toBe(true);
    expect(await store.claim("event-1", 200)).toBe(false);
    expect(await store.claim("event-1", 1200)).toBe(true);
  });

  it("prunes oldest rows when capacity is exceeded", async () => {
    const store = new RecentEventStore({ ttlMs: 10_000, maxEntries: 2 });
    await store.claim("event-1", 1);
    await store.claim("event-2", 2);
    await store.claim("event-3", 3);

    expect(await store.has("event-1", 4)).toBe(false);
    expect(await store.has("event-2", 4)).toBe(true);
    expect(await store.has("event-3", 4)).toBe(true);
  });
});
