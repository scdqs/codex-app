import {
  ALERT_KINDS,
  isAlertEvent,
  type AlertEvent,
  type AlertKind,
} from "@codex/bridge-protocol";

const PUSH_PAYLOAD_KEYS = [
  "eventId",
  "kind",
  "threadId",
  "threadTitle",
  "occurredAt",
  "vibrationEnabled",
  "vibrationPattern",
  "silent",
  "forceSystemNotification",
] as const;

export interface PushPayload extends AlertEvent {
  vibrationEnabled: boolean;
  vibrationPattern: number[];
  silent: boolean;
  forceSystemNotification: boolean;
}

export type AlertClientMessage = {
  type: "codex_alert_event";
  payload: AlertEvent;
};

export type OpenThreadMessage = {
  type: "open_thread";
  threadId: string;
};

export function parsePushPayload(value: unknown): PushPayload | null {
  if (!isRecord(value) || !hasExactKeys(value, PUSH_PAYLOAD_KEYS)) {
    return null;
  }
  if (
    !isAlertEvent(value) ||
    value.eventId.length === 0 ||
    value.eventId.length > 256 ||
    value.threadId.length === 0 ||
    value.threadId.length > 256 ||
    value.threadTitle.length > 200 ||
    typeof value.vibrationEnabled !== "boolean" ||
    !Array.isArray(value.vibrationPattern) ||
    value.vibrationPattern.length > 7 ||
    !value.vibrationPattern.every(
      (duration) =>
        typeof duration === "number" &&
        Number.isInteger(duration) &&
        duration >= 0 &&
        duration <= 1000,
    ) ||
    typeof value.silent !== "boolean" ||
    typeof value.forceSystemNotification !== "boolean"
  ) {
    return null;
  }
  return value as unknown as PushPayload;
}

export function notificationCopy(kind: AlertKind): { body: string } {
  return {
    completed: { body: "Codex task completed" },
    approval_required: { body: "Codex is waiting for approval" },
    input_required: { body: "Codex needs more input" },
    error: { body: "Codex task stopped with an error" },
  }[kind];
}

export function alertEventFromPush(payload: PushPayload): AlertEvent {
  return {
    eventId: payload.eventId,
    kind: payload.kind,
    threadId: payload.threadId,
    threadTitle: payload.threadTitle,
    occurredAt: payload.occurredAt,
  };
}

export function isAlertClientMessage(value: unknown): value is AlertClientMessage {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["type", "payload"] as const) &&
    value.type === "codex_alert_event" &&
    isAlertEvent(value.payload)
  );
}

export function isOpenThreadMessage(value: unknown): value is OpenThreadMessage {
  return (
    isRecord(value) &&
    hasExactKeys(value, ["type", "threadId"] as const) &&
    value.type === "open_thread" &&
    typeof value.threadId === "string" &&
    value.threadId.length > 0 &&
    value.threadId.length <= 256
  );
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value && typeof value === "object" && !Array.isArray(value));
}

function hasExactKeys<const Keys extends readonly string[]>(
  value: Record<string, unknown>,
  expected: Keys,
): boolean {
  const actual = Object.keys(value).sort();
  const sortedExpected = [...expected].sort();
  return (
    actual.length === sortedExpected.length &&
    actual.every((key, index) => key === sortedExpected[index])
  );
}

export { ALERT_KINDS };
