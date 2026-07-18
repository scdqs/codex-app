import { describe, expect, it } from "vitest";
import {
  type BridgeHealth,
  isServerEnvelope,
  isApiErrorCode,
  isSessionDataEnabled,
  isSessionEventType,
  isSessionStatus,
  isWorkspaceOption,
  mapHealthToConnection,
  parseServerEnvelope,
  secondaryStatusText,
} from "@codex/bridge-protocol";

function bridgeHealth(status: string, connectionState: string): BridgeHealth {
  return {
    status,
    connectionState,
    instanceId: "bridge-instance-test",
  };
}

type IsRequired<T, K extends keyof T> = {} extends Pick<T, K> ? false : true;
const bridgeInstanceIdIsRequired: IsRequired<BridgeHealth, "instanceId"> = true;

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

  it("accepts all four Rust-compatible alert event kinds", () => {
    for (const kind of ["completed", "approval_required", "input_required", "error"] as const) {
      expect(
        isServerEnvelope({
          type: "alert_event",
          payload: {
            eventId: `event-${kind}`,
            kind,
            threadId: "thread-1",
            threadTitle: "Task",
            occurredAt: 1_784_349_000_000,
          },
        }),
      ).toBe(true);
    }
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
    expect(isSessionEventType("reasoning_summary")).toBe(true);
    expect(isSessionEventType("reasoning_summary_delta")).toBe(true);
    expect(isSessionEventType("plan")).toBe(true);
    expect(isSessionEventType("plan_delta")).toBe(true);
    expect(isSessionEventType("approval_requested")).toBe(true);
  });

  it("accepts Rust-compatible workspace options and API error codes", () => {
    expect(isWorkspaceOption({ cwd: "/Users/damon/Documents/my_ai/codex-app" })).toBe(true);
    expect(isWorkspaceOption({ cwd: 42 })).toBe(false);
    expect(isApiErrorCode("workspace_required")).toBe(true);
    expect(isApiErrorCode("workspace_not_allowed")).toBe(true);
    expect(isApiErrorCode("invalid_pairing_token")).toBe(true);
    expect(isApiErrorCode("push_unavailable")).toBe(true);
    expect(isApiErrorCode("invalid_subscription")).toBe(true);
    expect(isApiErrorCode("unknown_error")).toBe(false);
  });

  it("maps bridge health states to shared user-facing connection states", () => {
    expect(mapHealthToConnection(bridgeHealth("ok", "writable"))).toEqual({ label: "Writable" });
    expect(mapHealthToConnection(bridgeHealth("ok", "read-only"))).toEqual({ label: "Read-only" });
    expect(mapHealthToConnection(bridgeHealth("degraded", "inject_failed"))).toEqual({
      label: "Inject failed",
    });
    expect(mapHealthToConnection(bridgeHealth("degraded", "codex_not_running"))).toEqual({
      label: "ChatGPT/Codex not running",
    });
    expect(mapHealthToConnection(bridgeHealth("degraded", "mystery"))).toEqual({
      label: "Connection error",
      detail: "mystery",
    });
  });

  it("requires bridge instance identity in health payloads", () => {
    expect(bridgeInstanceIdIsRequired).toBe(true);
    expect(bridgeHealth("ok", "writable").instanceId).toBe("bridge-instance-test");
  });

  it("keeps secondary text and data-enabled state in one shared module", () => {
    expect(secondaryStatusText("Writable")).toBe("Writable");
    expect(secondaryStatusText("Inject failed")).toBe("Desktop bridge unavailable");
    expect(secondaryStatusText("ChatGPT/Codex not running")).toBe("Start desktop app");
    expect(secondaryStatusText("Reconnecting")).toBe("Retrying automatically");
    expect(secondaryStatusText("Connection error")).toBe("Needs new link");
    expect(isSessionDataEnabled("Writable")).toBe(true);
    expect(isSessionDataEnabled("Read-only")).toBe(true);
    expect(isSessionDataEnabled("Reconnecting")).toBe(true);
    expect(isSessionDataEnabled("Connection error")).toBe(false);
  });
});
