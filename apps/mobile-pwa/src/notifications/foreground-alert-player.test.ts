import { describe, expect, it, vi } from "vitest";
import type { AlertEvent, AlertKind } from "@codex/bridge-protocol";
import {
  ALERT_TONES,
  ForegroundAlertPlayer,
  type ToneEngine,
} from "./foreground-alert-player";
import type { DeviceNotificationSettings } from "./api";

describe("foreground alert player", () => {
  it("maps every alert kind to a distinct preset tone", () => {
    expect(new Set(Object.values(ALERT_TONES).map((tone) => JSON.stringify(tone))).size).toBe(4);
  });

  it("plays and vibrates one foreground alert only once per event id", async () => {
    const tone = new RecordingToneEngine();
    const vibration = vi.fn();
    const player = new ForegroundAlertPlayer(tone, vibration, () => "visible");

    await player.handle(alert("event-1", "approval_required"), enabledSettings());
    await player.handle(alert("event-1", "approval_required"), enabledSettings());

    expect(tone.played).toEqual(["approval_required"]);
    expect(vibration).toHaveBeenCalledTimes(1);
  });

  it("preview plays even when global sound is disabled", async () => {
    const tone = new RecordingToneEngine();
    const player = new ForegroundAlertPlayer(tone, vi.fn(), () => "visible");

    await player.preview("error");

    expect(tone.played).toEqual(["error"]);
  });
});

class RecordingToneEngine implements ToneEngine {
  played: AlertKind[] = [];
  async unlock() {}
  async play(kind: AlertKind) {
    this.played.push(kind);
  }
}

function alert(eventId: string, kind: AlertKind): AlertEvent {
  return {
    eventId,
    kind,
    threadId: "thread-1",
    threadTitle: "Task",
    occurredAt: 1,
  };
}

function enabledSettings(): DeviceNotificationSettings {
  return {
    enabled: true,
    alertKinds: {
      completed: true,
      approvalRequired: true,
      inputRequired: true,
      error: true,
    },
    soundEnabled: true,
    vibrationEnabled: true,
  };
}
