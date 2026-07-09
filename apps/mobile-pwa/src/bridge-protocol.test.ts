import { describe, expect, it } from "vitest";
import {
  isServerEnvelope,
  isSessionDataEnabled,
  isSessionEventType,
  isSessionStatus,
  mapHealthToConnection,
  parseServerEnvelope,
  secondaryStatusText,
} from "@codex/bridge-protocol";

describe("shared bridge protocol", () => {
  it("accepts Rust-compatible server envelopes", () => {
    const envelope = {
      type: "session_event",
      payload: {
        id: "event-1",
        threadId: "thread-1",
        type: "message_delta",
        payload: { role: "assistant", text: "hello" },
        createdAt: 1_725_000_000_001,
      },
    };

    expect(isServerEnvelope(envelope)).toBe(true);
    expect(parseServerEnvelope(JSON.stringify(envelope))).toEqual(envelope);
  });

  it("rejects unknown enum values before UI code consumes them", () => {
    expect(isSessionStatus("waiting")).toBe(false);
    expect(isSessionEventType("tool")).toBe(false);
    expect(
      parseServerEnvelope(JSON.stringify({ type: "session_snapshot", payload: { status: "waiting" } })),
    ).toBeNull();
  });

  it("keeps current Rust enum spellings explicit", () => {
    expect(isSessionStatus("idle")).toBe(true);
    expect(isSessionStatus("running")).toBe(true);
    expect(isSessionStatus("waiting_for_input")).toBe(true);
    expect(isSessionStatus("waiting_for_approval")).toBe(true);
    expect(isSessionStatus("error")).toBe(true);
    expect(isSessionEventType("message_delta")).toBe(true);
    expect(isSessionEventType("approval_requested")).toBe(true);
  });

  it("maps bridge health states to shared user-facing connection states", () => {
    expect(mapHealthToConnection({ status: "ok", connectionState: "writable" })).toEqual({ label: "Writable" });
    expect(mapHealthToConnection({ status: "ok", connectionState: "read-only" })).toEqual({ label: "Read-only" });
    expect(mapHealthToConnection({ status: "degraded", connectionState: "inject_failed" })).toEqual({
      label: "Inject failed",
    });
    expect(mapHealthToConnection({ status: "degraded", connectionState: "codex_not_running" })).toEqual({
      label: "Codex not running",
    });
    expect(mapHealthToConnection({ status: "degraded", connectionState: "mystery" })).toEqual({
      label: "Connection error",
      detail: "mystery",
    });
  });

  it("keeps secondary text and data-enabled state in one shared module", () => {
    expect(secondaryStatusText("Writable")).toBe("Writable");
    expect(secondaryStatusText("Inject failed")).toBe("Desktop bridge unavailable");
    expect(secondaryStatusText("Connection error")).toBe("Needs new link");
    expect(isSessionDataEnabled("Writable")).toBe(true);
    expect(isSessionDataEnabled("Read-only")).toBe(true);
    expect(isSessionDataEnabled("Connection error")).toBe(false);
  });
});
