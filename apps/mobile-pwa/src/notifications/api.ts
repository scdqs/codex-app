import {
  ALERT_KINDS,
  isAlertEvent,
  type AlertEvent,
  type AlertKind,
} from "@codex/bridge-protocol";
import type { DeviceSession } from "../storage";

export interface AlertKindSettings {
  completed: boolean;
  approvalRequired: boolean;
  inputRequired: boolean;
  error: boolean;
}

export interface DeviceNotificationSettings {
  enabled: boolean;
  alertKinds: AlertKindSettings;
  soundEnabled: boolean;
  vibrationEnabled: boolean;
}

export type NotificationSettingsInput = DeviceNotificationSettings;

export interface NotificationCapabilities {
  deliveryMode: "foreground_only" | "web_push";
  fixedHttps: boolean;
  systemNotifications: boolean;
  foregroundSound: boolean;
  foregroundVibration: boolean;
  vibrationControlledBySystem: boolean;
}

export interface NotificationSettingsResponse {
  settings: DeviceNotificationSettings;
  capabilities: NotificationCapabilities;
  subscriptionState: PushSubscriptionState;
}

export type PushSubscriptionState = "unavailable" | "not_enabled" | "active" | "needs_repair";

export async function getNotificationSettings(
  session: DeviceSession,
): Promise<NotificationSettingsResponse> {
  const response = await fetch(notificationUrl(session, "/api/notification-settings"), {
    headers: authorizationHeaders(session),
  });
  return parseResponse(response, isNotificationSettingsResponse, "notification settings");
}

export async function putNotificationSettings(
  session: DeviceSession,
  settings: NotificationSettingsInput,
): Promise<NotificationSettingsResponse> {
  const response = await fetch(notificationUrl(session, "/api/notification-settings"), {
    method: "PUT",
    headers: {
      ...authorizationHeaders(session),
      "Content-Type": "application/json",
    },
    body: JSON.stringify(settings),
  });
  return parseResponse(response, isNotificationSettingsResponse, "notification settings");
}

export async function sendTestAlert(session: DeviceSession): Promise<AlertEvent> {
  const response = await fetch(notificationUrl(session, "/api/notifications/test"), {
    method: "POST",
    headers: authorizationHeaders(session),
  });
  return parseResponse(response, isAlertEvent, "test alert");
}

export async function getPushPublicKey(session: DeviceSession): Promise<string> {
  const response = await fetch(notificationUrl(session, "/api/push/public-key"), {
    headers: authorizationHeaders(session),
  });
  return parseResponse(
    response,
    (value): value is { publicKey: string } =>
      isRecord(value) && typeof value.publicKey === "string" && value.publicKey.length > 0,
    "push public key",
  ).then((value) => value.publicKey);
}

export async function savePushSubscription(
  session: DeviceSession,
  origin: string,
  subscription: PushSubscriptionJSON,
): Promise<void> {
  if (
    typeof subscription.endpoint !== "string" ||
    !subscription.keys ||
    typeof subscription.keys.p256dh !== "string" ||
    typeof subscription.keys.auth !== "string"
  ) {
    throw new Error("Invalid PushSubscription");
  }
  const response = await fetch(notificationUrl(session, "/api/push/subscription"), {
    method: "POST",
    headers: {
      ...authorizationHeaders(session),
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      origin,
      endpoint: subscription.endpoint,
      keys: {
        p256dh: subscription.keys.p256dh,
        auth: subscription.keys.auth,
      },
    }),
  });
  await requireOk(response, "push subscription");
}

export async function deletePushSubscription(session: DeviceSession): Promise<void> {
  const response = await fetch(notificationUrl(session, "/api/push/subscription"), {
    method: "DELETE",
    headers: authorizationHeaders(session),
  });
  await requireOk(response, "push subscription cleanup");
}

export function isKindEnabled(settings: DeviceNotificationSettings, kind: AlertKind): boolean {
  switch (kind) {
    case "completed":
      return settings.alertKinds.completed;
    case "approval_required":
      return settings.alertKinds.approvalRequired;
    case "input_required":
      return settings.alertKinds.inputRequired;
    case "error":
      return settings.alertKinds.error;
  }
}

function authorizationHeaders(session: DeviceSession): Record<string, string> {
  return { Authorization: `Bearer ${session.sessionToken}` };
}

function notificationUrl(session: DeviceSession, path: string): string {
  return new URL(path, session.bridgeUrl ?? window.location.origin).toString();
}

async function parseResponse<T>(
  response: Response,
  guard: (value: unknown) => value is T,
  label: string,
): Promise<T> {
  if (!response.ok) {
    throw new Error(`${label} request failed with ${response.status}`);
  }
  const value: unknown = await response.json();
  if (!guard(value)) {
    throw new Error(`Invalid ${label} response`);
  }
  return value;
}

async function requireOk(response: Response, label: string): Promise<void> {
  if (!response.ok) {
    throw new Error(`${label} request failed with ${response.status}`);
  }
}

function isNotificationSettingsResponse(value: unknown): value is NotificationSettingsResponse {
  if (!isRecord(value)) {
    return false;
  }
  return (
    isDeviceNotificationSettings(value.settings) &&
    isNotificationCapabilities(value.capabilities) &&
    includes(
      ["unavailable", "not_enabled", "active", "needs_repair"] as const,
      value.subscriptionState,
    )
  );
}

function isDeviceNotificationSettings(value: unknown): value is DeviceNotificationSettings {
  if (!isRecord(value) || !isRecord(value.alertKinds)) {
    return false;
  }
  return (
    typeof value.enabled === "boolean" &&
    typeof value.alertKinds.completed === "boolean" &&
    typeof value.alertKinds.approvalRequired === "boolean" &&
    typeof value.alertKinds.inputRequired === "boolean" &&
    typeof value.alertKinds.error === "boolean" &&
    typeof value.soundEnabled === "boolean" &&
    typeof value.vibrationEnabled === "boolean"
  );
}

function isNotificationCapabilities(value: unknown): value is NotificationCapabilities {
  if (!isRecord(value)) {
    return false;
  }
  return (
    includes(["foreground_only", "web_push"] as const, value.deliveryMode) &&
    typeof value.fixedHttps === "boolean" &&
    typeof value.systemNotifications === "boolean" &&
    typeof value.foregroundSound === "boolean" &&
    typeof value.foregroundVibration === "boolean" &&
    typeof value.vibrationControlledBySystem === "boolean"
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function includes<const T extends readonly string[]>(values: T, value: unknown): value is T[number] {
  return typeof value === "string" && (values as readonly string[]).includes(value);
}

export { ALERT_KINDS };
