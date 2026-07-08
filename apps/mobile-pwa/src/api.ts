import type { DeviceSession } from "./storage";

export interface PairingPayload {
  pairingToken: string;
  bridgeUrl?: string;
  displayName?: string;
}

export interface CompletePairingRequest {
  pairingToken: string;
  deviceId: string;
  displayName: string;
  deviceSecret: string;
}

export interface SessionResponse {
  deviceId: string;
  sessionToken: string;
  sessionExpiresAt: string;
}

export interface HealthResponse {
  status: string;
  connectionState: string;
}

export function readPairingPayloadFromUrl(url: string): PairingPayload | null {
  const parsed = new URL(url, window.location.href);
  const pairingToken = parsed.searchParams.get("pairingToken") ?? parsed.searchParams.get("token");
  if (!pairingToken) {
    return null;
  }

  const bridgeUrl = parsed.searchParams.get("bridgeUrl") ?? undefined;
  const displayName =
    parsed.searchParams.get("displayName") ?? parsed.searchParams.get("deviceName") ?? undefined;

  return {
    pairingToken,
    bridgeUrl,
    displayName,
  };
}

export async function completePairing(
  bridgeUrl: string,
  request: CompletePairingRequest,
): Promise<SessionResponse> {
  return postJson<SessionResponse>(bridgeUrl, "/api/pairing/complete", request);
}

export async function refreshSession(
  bridgeUrl: string,
  request: Pick<DeviceSession, "deviceId" | "deviceSecret">,
): Promise<SessionResponse> {
  return postJson<SessionResponse>(bridgeUrl, "/api/session/refresh", request);
}

export async function getHealth(bridgeUrl: string, sessionToken?: string): Promise<HealthResponse> {
  const response = await fetch(apiUrl(bridgeUrl, "/api/health"), {
    headers: sessionToken ? { Authorization: `Bearer ${sessionToken}` } : undefined,
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Health request failed with ${response.status}`);
  }

  return (await response.json()) as HealthResponse;
}

export function connectWebSocket(bridgeUrl: string, sessionToken: string): WebSocket {
  const url = new URL("/ws", bridgeUrl);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  url.searchParams.set("token", sessionToken);
  return new WebSocket(url.toString());
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

async function postJson<T>(bridgeUrl: string, path: string, body: unknown): Promise<T> {
  const response = await fetch(apiUrl(bridgeUrl, path), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Request failed with ${response.status}`);
  }

  return (await response.json()) as T;
}

function apiUrl(bridgeUrl: string, path: string): string {
  return new URL(path, bridgeUrl).toString();
}
