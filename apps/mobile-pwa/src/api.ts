import type { DeviceSession } from "./storage";
import type { DecisionKind, SessionEvent, SessionSnapshot } from "./protocol";

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
  sessionExpiresAt: number;
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
  return postJson(bridgeUrl, "/api/pairing/complete", request, parseSessionResponse);
}

export async function refreshSession(
  bridgeUrl: string,
  request: Pick<DeviceSession, "deviceId" | "deviceSecret">,
): Promise<SessionResponse> {
  return postJson(bridgeUrl, "/api/session/refresh", request, parseSessionResponse);
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

export async function listSessions(
  bridgeUrl: string,
  sessionToken: string,
): Promise<SessionSnapshot[]> {
  const response = await fetch(apiUrl(bridgeUrl, "/api/sessions"), {
    headers: { Authorization: `Bearer ${sessionToken}` },
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Sessions request failed with ${response.status}`);
  }

  return parseSessionSnapshots(await response.json());
}

export async function listSessionEvents(
  bridgeUrl: string,
  sessionToken: string,
  threadId: string,
): Promise<SessionEvent[]> {
  const response = await fetch(apiUrl(bridgeUrl, `/api/sessions/${encodeURIComponent(threadId)}/events`), {
    headers: { Authorization: `Bearer ${sessionToken}` },
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Session events request failed with ${response.status}`);
  }

  return parseSessionEvents(await response.json());
}

export async function fetchAssetBlob(
  bridgeUrl: string,
  sessionToken: string,
  src: string,
): Promise<Blob> {
  const assetUrl = allowedAssetUrl(bridgeUrl, src);
  if (!assetUrl) {
    throw new ApiError(400, "Invalid asset source");
  }

  const response = await fetch(assetUrl, {
    headers: { Authorization: `Bearer ${sessionToken}` },
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Asset request failed with ${response.status}`);
  }

  const contentType = response.headers.get("Content-Type") || "";
  if (!contentType.startsWith("image/")) {
    throw new ApiError(response.status, "Asset response is not an image");
  }

  return response.blob();
}

function allowedAssetUrl(bridgeUrl: string, src: string): string | null {
  if (src.startsWith("//") || /[a-zA-Z][a-zA-Z\d+.-]*:\/\//.test(src)) {
    return null;
  }

  const bridge = new URL(bridgeUrl);
  const url = new URL(src, bridge);
  if (url.origin !== bridge.origin || !url.pathname.startsWith("/api/assets/")) {
    return null;
  }

  return url.toString();
}

export async function sendTextMessage(
  bridgeUrl: string,
  sessionToken: string,
  threadId: string,
  text: string,
): Promise<void> {
  const response = await fetch(apiUrl(bridgeUrl, `/api/sessions/${encodeURIComponent(threadId)}/messages`), {
    method: "POST",
    headers: {
      Authorization: `Bearer ${sessionToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ text }),
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Send message request failed with ${response.status}`);
  }
}

export async function decideApproval(
  bridgeUrl: string,
  sessionToken: string,
  approvalId: string,
  decision: DecisionKind,
): Promise<void> {
  const response = await fetch(apiUrl(bridgeUrl, `/api/approvals/${encodeURIComponent(approvalId)}/decision`), {
    method: "POST",
    headers: {
      Authorization: `Bearer ${sessionToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ decision }),
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Approval decision request failed with ${response.status}`);
  }
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

export class ApiValidationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ApiValidationError";
  }
}

async function postJson<T>(
  bridgeUrl: string,
  path: string,
  body: unknown,
  parse: (value: unknown) => T,
): Promise<T> {
  const response = await fetch(apiUrl(bridgeUrl, path), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Request failed with ${response.status}`);
  }

  return parse(await response.json());
}

function apiUrl(bridgeUrl: string, path: string): string {
  return new URL(path, bridgeUrl).toString();
}

function parseSessionResponse(value: unknown): SessionResponse {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ApiValidationError("Session response must be an object");
  }

  const response = value as Record<string, unknown>;
  if (
    typeof response.deviceId !== "string" ||
    typeof response.sessionToken !== "string" ||
    typeof response.sessionExpiresAt !== "number" ||
    !Number.isFinite(response.sessionExpiresAt)
  ) {
    throw new ApiValidationError("Session response is missing required fields");
  }

  return {
    deviceId: response.deviceId,
    sessionToken: response.sessionToken,
    sessionExpiresAt: response.sessionExpiresAt,
  };
}

function parseSessionSnapshots(value: unknown): SessionSnapshot[] {
  if (!Array.isArray(value)) {
    throw new ApiValidationError("Sessions response must be an array");
  }
  return value.map(parseSessionSnapshot);
}

function parseSessionSnapshot(value: unknown): SessionSnapshot {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ApiValidationError("Session snapshot must be an object");
  }

  const session = value as Record<string, unknown>;
  if (
    typeof session.threadId !== "string" ||
    typeof session.title !== "string" ||
    typeof session.updatedAt !== "number" ||
    !Number.isFinite(session.updatedAt) ||
    !isSessionStatus(session.status) ||
    !Array.isArray(session.pendingApprovalIds) ||
    !session.pendingApprovalIds.every((id) => typeof id === "string")
  ) {
    throw new ApiValidationError("Session snapshot is missing required fields");
  }

  return {
    threadId: session.threadId,
    title: session.title,
    cwd: typeof session.cwd === "string" ? session.cwd : undefined,
    modelProvider: typeof session.modelProvider === "string" ? session.modelProvider : undefined,
    preview: typeof session.preview === "string" ? session.preview : undefined,
    updatedAt: session.updatedAt,
    status: session.status,
    pendingApprovalIds: session.pendingApprovalIds,
  };
}

function parseSessionEvents(value: unknown): SessionEvent[] {
  if (!Array.isArray(value)) {
    throw new ApiValidationError("Session events response must be an array");
  }
  return value.map(parseSessionEvent);
}

function parseSessionEvent(value: unknown): SessionEvent {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ApiValidationError("Session event must be an object");
  }

  const event = value as Record<string, unknown>;
  if (
    typeof event.id !== "string" ||
    typeof event.threadId !== "string" ||
    typeof event.createdAt !== "number" ||
    !Number.isFinite(event.createdAt) ||
    !isSessionEventType(event.type) ||
    !isJsonValue(event.payload)
  ) {
    throw new ApiValidationError("Session event is missing required fields");
  }

  return {
    id: event.id,
    threadId: event.threadId,
    type: event.type,
    payload: event.payload,
    createdAt: event.createdAt,
  };
}

function isSessionStatus(value: unknown): value is SessionSnapshot["status"] {
  return (
    value === "idle" ||
    value === "running" ||
    value === "waiting_for_input" ||
    value === "waiting_for_approval" ||
    value === "error"
  );
}

function isSessionEventType(value: unknown): value is SessionEvent["type"] {
  return (
    value === "message" ||
    value === "message_delta" ||
    value === "tool_call" ||
    value === "tool_result" ||
    value === "approval_requested" ||
    value === "approval_resolved" ||
    value === "status_changed" ||
    value === "error"
  );
}

function isJsonValue(value: unknown): value is SessionEvent["payload"] {
  if (
    value === null ||
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  ) {
    return true;
  }
  if (Array.isArray(value)) {
    return value.every(isJsonValue);
  }
  if (typeof value === "object") {
    return Object.values(value).every(isJsonValue);
  }
  return false;
}
