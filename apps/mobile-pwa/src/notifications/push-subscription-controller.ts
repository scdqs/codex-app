import type { PushCapabilities } from "./capabilities";
import type { PushSubscriptionState } from "./api";

export interface BrowserPushSubscription {
  toJSON(): PushSubscriptionJSON;
  unsubscribe(): Promise<boolean>;
}

export interface PushSubscriptionPorts {
  permission(): NotificationPermission;
  requestPermission(): Promise<NotificationPermission>;
  getPublicKey(): Promise<string>;
  getSubscription(): Promise<BrowserPushSubscription | null>;
  subscribe(options: PushSubscriptionOptionsInit): Promise<BrowserPushSubscription>;
  saveSubscription(input: {
    origin: string;
    subscription: PushSubscriptionJSON;
  }): Promise<void>;
  deleteServerSubscription(): Promise<void>;
  origin(): string;
}

export type SystemNotificationState =
  | "active"
  | "not_enabled"
  | "blocked"
  | "needs_repair"
  | "unavailable";

export type PushSubscriptionErrorCode =
  | "push_unavailable"
  | "ios_install_required"
  | "unsupported"
  | "permission_denied"
  | "disable_failed";

export class PushSubscriptionError extends Error {
  constructor(readonly code: PushSubscriptionErrorCode) {
    super(pushErrorMessage(code));
    this.name = "PushSubscriptionError";
  }
}

export class PushSubscriptionController {
  constructor(private readonly ports: PushSubscriptionPorts) {}

  async enable(capabilities: PushCapabilities): Promise<void> {
    assertPushCanStart(capabilities);
    if (this.ports.permission() === "denied") {
      throw pushError("permission_denied");
    }
    const permission =
      this.ports.permission() === "granted"
        ? "granted"
        : await this.ports.requestPermission();
    if (permission !== "granted") {
      throw pushError("permission_denied");
    }

    const publicKey = await this.ports.getPublicKey();
    const existing = await this.ports.getSubscription();
    const subscription =
      existing ??
      (await this.ports.subscribe({
        userVisibleOnly: true,
        applicationServerKey: urlBase64ToUint8Array(publicKey),
      }));
    await this.ports.saveSubscription({
      origin: this.ports.origin(),
      subscription: subscription.toJSON(),
    });
  }

  async repair(capabilities: PushCapabilities): Promise<void> {
    assertPushCanStart(capabilities);
    await this.ports.deleteServerSubscription();
    const existing = await this.ports.getSubscription();
    if (existing) {
      await existing.unsubscribe();
    }
    await this.enable(capabilities);
  }

  async disable(): Promise<void> {
    let failed = false;
    try {
      await this.ports.deleteServerSubscription();
    } catch {
      failed = true;
    }
    try {
      const existing = await this.ports.getSubscription();
      if (existing) {
        await existing.unsubscribe();
      }
    } catch {
      failed = true;
    }
    if (failed) {
      throw pushError("disable_failed");
    }
  }

  async inspect(
    capabilities: PushCapabilities,
    serverState: PushSubscriptionState,
  ): Promise<SystemNotificationState> {
    if (serverState === "unavailable" || !hasPushSupport(capabilities)) {
      return "unavailable";
    }
    if (capabilities.isIos && !capabilities.standalone) {
      return "unavailable";
    }
    const permission = this.ports.permission();
    if (permission === "denied") {
      return "blocked";
    }
    let existing: BrowserPushSubscription | null;
    try {
      existing = await this.ports.getSubscription();
    } catch {
      return "needs_repair";
    }
    if (permission === "granted" && existing && serverState === "active") {
      return "active";
    }
    if (
      serverState === "needs_repair" ||
      serverState === "active" ||
      existing ||
      permission === "granted"
    ) {
      return "needs_repair";
    }
    return "not_enabled";
  }
}

function assertPushCanStart(capabilities: PushCapabilities): void {
  if (!capabilities.fixedHttps) {
    throw pushError("push_unavailable");
  }
  if (capabilities.isIos && !capabilities.standalone) {
    throw pushError("ios_install_required");
  }
  if (!hasPushSupport(capabilities)) {
    throw pushError("unsupported");
  }
}

function hasPushSupport(capabilities: PushCapabilities): boolean {
  return (
    capabilities.fixedHttps &&
    capabilities.secureContext &&
    capabilities.serviceWorker &&
    capabilities.pushManager &&
    capabilities.notificationApi
  );
}

function urlBase64ToUint8Array(value: string): Uint8Array<ArrayBuffer> {
  const padded = value.padEnd(value.length + ((4 - (value.length % 4)) % 4), "=");
  const raw = globalThis.atob(padded.replaceAll("-", "+").replaceAll("_", "/"));
  const bytes = new Uint8Array(new ArrayBuffer(raw.length));
  for (let index = 0; index < raw.length; index += 1) {
    bytes[index] = raw.charCodeAt(index);
  }
  return bytes;
}

function pushError(code: PushSubscriptionErrorCode): PushSubscriptionError {
  return new PushSubscriptionError(code);
}

function pushErrorMessage(code: PushSubscriptionErrorCode): string {
  return {
    push_unavailable: "Lock-screen alerts require a fixed HTTPS address",
    ios_install_required: "Add this app to the Home Screen before enabling system notifications",
    unsupported: "System notifications are not supported by this browser",
    permission_denied: "System notifications are blocked in browser settings",
    disable_failed: "Alerts were disabled, but notification cleanup needs attention",
  }[code];
}
