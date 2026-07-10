export interface BridgeHealth {
  status: string;
  connectionState: string;
}

export type ConnectionLabel =
  | "Unpaired"
  | "Pairing"
  | "Connected"
  | "ChatGPT/Codex not running"
  | "Inject failed"
  | "Read-only"
  | "Writable"
  | "Connection error";

export interface ConnectionViewState {
  label: ConnectionLabel;
  detail?: string;
}

export function mapHealthToConnection(health: BridgeHealth): ConnectionViewState {
  const state = normalizeConnectionState(health.connectionState);
  if (state === "codex_not_running" || state === "not_running") {
    return { label: "ChatGPT/Codex not running" };
  }
  if (state === "inject_failed" || state === "injection_failed") {
    return { label: "Inject failed" };
  }
  if (state === "read_only" || state === "readonly") {
    return { label: "Read-only" };
  }
  if (state === "writable" || state === "ready") {
    return { label: "Writable" };
  }
  if (state === "connected" || health.status.toLowerCase() === "ok") {
    return { label: "Connected" };
  }
  return { label: "Connection error", detail: health.connectionState };
}

export function secondaryStatusText(label: ConnectionLabel): string {
  switch (label) {
    case "Connected":
    case "Writable":
      return "Writable";
    case "Read-only":
      return "Read-only";
    case "Inject failed":
      return "Desktop bridge unavailable";
    case "ChatGPT/Codex not running":
      return "Start desktop app";
    case "Connection error":
      return "Needs new link";
    case "Pairing":
      return "Pairing";
    case "Unpaired":
      return "Open pairing link";
  }
}

export function isSessionDataEnabled(label: ConnectionLabel): boolean {
  return label === "Connected" || label === "Writable" || label === "Read-only";
}

function normalizeConnectionState(value: string): string {
  return value.toLowerCase().replaceAll("-", "_");
}
