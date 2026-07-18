import type { AlertEvent, AlertKind } from "@codex/bridge-protocol";
import { isKindEnabled, type DeviceNotificationSettings } from "./api";

export interface ToneStep {
  frequency: number;
  durationMs: number;
  gapMs: number;
}

export const ALERT_TONES: Record<AlertKind, readonly ToneStep[]> = {
  completed: [
    { frequency: 659, durationMs: 80, gapMs: 25 },
    { frequency: 880, durationMs: 110, gapMs: 0 },
  ],
  approval_required: [
    { frequency: 523, durationMs: 90, gapMs: 70 },
    { frequency: 659, durationMs: 90, gapMs: 0 },
  ],
  input_required: [
    { frequency: 740, durationMs: 65, gapMs: 45 },
    { frequency: 740, durationMs: 65, gapMs: 0 },
  ],
  error: [
    { frequency: 392, durationMs: 120, gapMs: 45 },
    { frequency: 311, durationMs: 150, gapMs: 0 },
  ],
};

export const ALERT_VIBRATION: Record<AlertKind, readonly number[]> = {
  completed: [80],
  approval_required: [80, 60, 80],
  input_required: [45, 40, 45],
  error: [150, 80, 150],
};

export interface ToneEngine {
  unlock(): Promise<void>;
  play(kind: AlertKind): Promise<void>;
}

export interface AlertPlaybackResult {
  played: boolean;
  duplicate: boolean;
  soundBlocked?: boolean;
}

export class ForegroundAlertPlayer {
  private readonly seen = new Map<string, number>();

  constructor(
    private readonly tone: ToneEngine,
    private readonly vibrate: (pattern: number[]) => void,
    private readonly visibility: () => DocumentVisibilityState,
  ) {}

  unlock(): Promise<void> {
    return this.tone.unlock();
  }

  async preview(kind: AlertKind): Promise<void> {
    await this.tone.unlock();
    await this.tone.play(kind);
  }

  async handle(
    event: AlertEvent,
    settings: DeviceNotificationSettings,
  ): Promise<AlertPlaybackResult> {
    if (this.seen.has(event.eventId)) {
      return { played: false, duplicate: true };
    }
    this.remember(event.eventId);
    if (
      this.visibility() !== "visible" ||
      !settings.enabled ||
      !isKindEnabled(settings, event.kind)
    ) {
      return { played: false, duplicate: false };
    }

    let soundBlocked = false;
    if (settings.soundEnabled) {
      try {
        await this.tone.play(event.kind);
      } catch {
        soundBlocked = true;
      }
    }
    if (settings.vibrationEnabled) {
      this.vibrate([...ALERT_VIBRATION[event.kind]]);
    }
    return { played: true, duplicate: false, soundBlocked };
  }

  private remember(eventId: string) {
    this.seen.set(eventId, Date.now());
    if (this.seen.size > 256) {
      const oldest = this.seen.keys().next().value;
      if (oldest) {
        this.seen.delete(oldest);
      }
    }
  }
}

export class WebAudioToneEngine implements ToneEngine {
  private context: AudioContext | null = null;

  async unlock(): Promise<void> {
    const context = this.audioContext();
    if (context.state === "suspended") {
      await context.resume();
    }
  }

  async play(kind: AlertKind): Promise<void> {
    const context = this.audioContext();
    if (context.state !== "running") {
      throw new Error("Audio context is locked");
    }
    let offset = 0;
    for (const step of ALERT_TONES[kind]) {
      const start = context.currentTime + offset / 1_000;
      const stop = start + step.durationMs / 1_000;
      const oscillator = context.createOscillator();
      const gain = context.createGain();
      oscillator.frequency.setValueAtTime(step.frequency, start);
      gain.gain.setValueAtTime(0.0001, start);
      gain.gain.exponentialRampToValueAtTime(0.06, start + 0.008);
      gain.gain.setValueAtTime(0.06, Math.max(start + 0.008, stop - 0.02));
      gain.gain.exponentialRampToValueAtTime(0.0001, stop);
      oscillator.connect(gain).connect(context.destination);
      oscillator.start(start);
      oscillator.stop(stop);
      offset += step.durationMs + step.gapMs;
    }
    await new Promise<void>((resolve) => window.setTimeout(resolve, offset));
  }

  private audioContext(): AudioContext {
    if (!this.context) {
      const AudioContextConstructor =
        window.AudioContext ??
        (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
      if (!AudioContextConstructor) {
        throw new Error("Web Audio is unavailable");
      }
      this.context = new AudioContextConstructor();
    }
    return this.context;
  }
}

export function createForegroundAlertPlayer(): ForegroundAlertPlayer {
  return new ForegroundAlertPlayer(
    new WebAudioToneEngine(),
    (pattern) => navigator.vibrate?.(pattern),
    () => document.visibilityState,
  );
}
