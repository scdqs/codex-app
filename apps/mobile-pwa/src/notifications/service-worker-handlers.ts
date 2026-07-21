import {
  alertEventFromPush,
  notificationCopy,
  parsePushPayload,
  type AlertClientMessage,
  type OpenThreadMessage,
} from "./push-protocol";

export interface ServiceWorkerNotificationOptions extends NotificationOptions {
  vibrate?: number[];
}

export interface WindowClientPort {
  readonly visibilityState: "hidden" | "visible" | "prerender";
  readonly url: string;
  postMessage(message: AlertClientMessage | OpenThreadMessage): void;
  focus(): Promise<WindowClientPort>;
}

export interface ServiceWorkerEnvironment {
  claimEvent(eventId: string, now: number): Promise<boolean>;
  visibleWindowClients(): Promise<WindowClientPort[]>;
  showNotification(title: string, options: ServiceWorkerNotificationOptions): Promise<void>;
  openWindow(url: string): Promise<WindowClientPort | null>;
  allWindowClients(): Promise<WindowClientPort[]>;
}

export async function handlePush(
  raw: unknown,
  environment: ServiceWorkerEnvironment,
  now = Date.now(),
): Promise<void> {
  const payload = parsePushPayload(raw);
  if (!payload) {
    console.warn("invalid_push_payload");
    return;
  }
  if (!(await environment.claimEvent(payload.eventId, now))) {
    return;
  }
  const visibleClients = await environment.visibleWindowClients();
  if (visibleClients.length > 0 && !payload.forceSystemNotification) {
    const message: AlertClientMessage = {
      type: "codex_alert_event",
      payload: alertEventFromPush(payload),
    };
    for (const client of visibleClients) {
      client.postMessage(message);
    }
    return;
  }
  await environment.showNotification(payload.threadTitle, {
    body: notificationCopy(payload.kind).body,
    tag: payload.eventId,
    icon: "/icon-192.png",
    badge: "/icon-192.png",
    data: { threadId: payload.threadId, eventId: payload.eventId },
    silent: payload.silent,
    vibrate: payload.vibrationEnabled ? payload.vibrationPattern : undefined,
  });
}

export async function handleNotificationClick(
  data: unknown,
  environment: ServiceWorkerEnvironment,
): Promise<void> {
  const threadId = notificationThreadId(data);
  if (!threadId) {
    return;
  }
  const clients = await environment.allWindowClients();
  const existing = clients[0];
  if (existing) {
    try {
      const focused = await existing.focus();
      focused.postMessage({ type: "open_thread", threadId });
    } catch {
      console.warn("notification_client_focus_failed");
    }
    return;
  }
  try {
    await environment.openWindow(`/?threadId=${encodeURIComponent(threadId)}`);
  } catch {
    console.warn("notification_window_open_failed");
  }
}

function notificationThreadId(value: unknown): string | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  const threadId = (value as Record<string, unknown>).threadId;
  return typeof threadId === "string" && threadId.length > 0 && threadId.length <= 256
    ? threadId
    : null;
}
