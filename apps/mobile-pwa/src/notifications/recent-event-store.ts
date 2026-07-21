const DATABASE_NAME = "codex-mobile-notifications-v1";
const OBJECT_STORE_NAME = "recent_alert_events";
const EXPIRES_INDEX_NAME = "expiresAt";
const DEFAULT_TTL_MS = 7 * 24 * 60 * 60 * 1000;
const DEFAULT_MAX_ENTRIES = 256;

interface RecentEventRow {
  eventId: string;
  occurredAt: number;
  expiresAt: number;
}

export class RecentEventStore {
  private readonly ttlMs: number;
  private readonly maxEntries: number;
  private database?: Promise<IDBDatabase>;

  constructor(options?: { ttlMs?: number; maxEntries?: number }) {
    this.ttlMs = options?.ttlMs ?? DEFAULT_TTL_MS;
    this.maxEntries = options?.maxEntries ?? DEFAULT_MAX_ENTRIES;
  }

  async claim(eventId: string, now = Date.now()): Promise<boolean> {
    try {
      const database = await this.open();
      return await new Promise<boolean>((resolve) => {
        const transaction = database.transaction(OBJECT_STORE_NAME, "readwrite");
        const store = transaction.objectStore(OBJECT_STORE_NAME);
        let claimed = true;
        const request = store.get(eventId);
        request.onsuccess = () => {
          const existing = request.result as RecentEventRow | undefined;
          if (existing && existing.expiresAt > now) {
            claimed = false;
            return;
          }
          store.put({ eventId, occurredAt: now, expiresAt: now + this.ttlMs } satisfies RecentEventRow);
          const rowsRequest = store.getAll();
          rowsRequest.onsuccess = () => {
            const rows = rowsRequest.result as RecentEventRow[];
            const expired = rows.filter((row) => row.expiresAt <= now);
            for (const row of expired) {
              if (row.eventId !== eventId) {
                store.delete(row.eventId);
              }
            }
            const active = rows
              .filter((row) => row.expiresAt > now && row.eventId !== eventId)
              .concat({ eventId, occurredAt: now, expiresAt: now + this.ttlMs })
              .sort((left, right) => left.occurredAt - right.occurredAt);
            for (const row of active.slice(0, Math.max(0, active.length - this.maxEntries))) {
              store.delete(row.eventId);
            }
          };
        };
        transaction.oncomplete = () => resolve(claimed);
        transaction.onerror = () => {
          console.warn("recent_alert_store_failed");
          resolve(true);
        };
        transaction.onabort = () => {
          console.warn("recent_alert_store_failed");
          resolve(true);
        };
      });
    } catch {
      console.warn("recent_alert_store_failed");
      return true;
    }
  }

  async has(eventId: string, now = Date.now()): Promise<boolean> {
    try {
      const database = await this.open();
      return await new Promise<boolean>((resolve, reject) => {
        const transaction = database.transaction(OBJECT_STORE_NAME, "readonly");
        const request = transaction.objectStore(OBJECT_STORE_NAME).get(eventId);
        request.onsuccess = () => {
          const row = request.result as RecentEventRow | undefined;
          resolve(Boolean(row && row.expiresAt > now));
        };
        request.onerror = () => reject(request.error);
      });
    } catch {
      console.warn("recent_alert_store_failed");
      return false;
    }
  }

  private open(): Promise<IDBDatabase> {
    this.database ??= new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open(DATABASE_NAME, 1);
      request.onupgradeneeded = () => {
        const database = request.result;
        const store = database.createObjectStore(OBJECT_STORE_NAME, { keyPath: "eventId" });
        store.createIndex(EXPIRES_INDEX_NAME, "expiresAt");
      };
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    return this.database;
  }
}
