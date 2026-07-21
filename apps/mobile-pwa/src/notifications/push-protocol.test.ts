import { describe, expect, it } from "vitest";
import {
  alertEventFromPush,
  isAlertClientMessage,
  isOpenThreadMessage,
  notificationCopy,
  parsePushPayload,
} from "./push-protocol";

const valid = {
  eventId: "alert-1",
  kind: "completed" as const,
  threadId: "thread-1",
  threadTitle: "Release",
  occurredAt: 1_784_349_000_000,
  vibrationEnabled: true,
  vibrationPattern: [80],
  silent: false,
  forceSystemNotification: false,
};

describe("push protocol", () => {
  it("accepts the minimal private payload and rejects extra sensitive fields", () => {
    expect(parsePushPayload(valid)).toEqual(valid);
    expect(parsePushPayload({ ...valid, cwd: "/secret/project" })).toBeNull();
    expect(parsePushPayload({ ...valid, reply: "private response" })).toBeNull();
    expect(parsePushPayload({ ...valid, vibrationPattern: [1001] })).toBeNull();
  });

  it("maps all four kinds to approved notification copy", () => {
    expect(notificationCopy("completed").body).toBe("Codex task completed");
    expect(notificationCopy("approval_required").body).toBe("Codex is waiting for approval");
    expect(notificationCopy("input_required").body).toBe("Codex needs more input");
    expect(notificationCopy("error").body).toBe("Codex task stopped with an error");
  });

  it("converts push payloads and validates worker messages", () => {
    const alert = alertEventFromPush(valid);
    expect(alert).not.toHaveProperty("silent");
    expect(isAlertClientMessage({ type: "codex_alert_event", payload: alert })).toBe(true);
    expect(isAlertClientMessage({ type: "codex_alert_event", payload: alert, cwd: "/secret" })).toBe(
      false,
    );
    expect(isOpenThreadMessage({ type: "open_thread", threadId: "thread-1" })).toBe(true);
  });
});
