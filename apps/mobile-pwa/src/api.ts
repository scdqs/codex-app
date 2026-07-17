import type { DeviceSession } from "./storage";
import {
  isApprovalRequest,
  isJsonValue,
  isSessionEventType,
  isSessionStatus,
  type BridgeHealth,
  type ApprovalRequest,
  type DecisionKind,
  type SessionEvent,
  type SessionSnapshot,
} from "@codex/bridge-protocol";

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

export type HealthResponse = BridgeHealth;

export interface OutgoingImageAttachment {
  name: string;
  mimeType: string;
  dataBase64: string;
}

export interface SessionEventPageOptions {
  limit?: number;
  before?: string;
  since?: string;
}

export interface SessionEventPage {
  events: SessionEvent[];
  beforeCursor?: string;
  afterCursor?: string;
  hasMoreBefore: boolean;
  hasMoreAfter: boolean;
  reset: boolean;
  legacySnapshot: boolean;
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

export async function listApprovals(
  bridgeUrl: string,
  sessionToken: string,
): Promise<ApprovalRequest[]> {
  const response = await fetch(apiUrl(bridgeUrl, "/api/approvals"), {
    headers: { Authorization: `Bearer ${sessionToken}` },
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Approvals request failed with ${response.status}`);
  }

  return parseApprovalRequests(await response.json());
}

export async function listSessionEvents(
  bridgeUrl: string,
  sessionToken: string,
  threadId: string,
  options: SessionEventPageOptions = {},
): Promise<SessionEventPage> {
  const headers: Record<string, string> = { Authorization: `Bearer ${sessionToken}` };
  if (options.limit !== undefined) {
    headers["X-Codex-Events-Limit"] = String(options.limit);
  }
  if (options.before) {
    headers["X-Codex-Events-Before"] = options.before;
  }
  if (options.since) {
    headers["X-Codex-Events-Since"] = options.since;
  }
  const response = await fetch(apiUrl(bridgeUrl, `/api/sessions/${encodeURIComponent(threadId)}/events`), {
    headers,
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Session events request failed with ${response.status}`);
  }

  return parseSessionEventPage(await response.json());
}

export async function createSession(
  bridgeUrl: string,
  sessionToken: string,
  text: string,
  attachments: OutgoingImageAttachment[] = [],
): Promise<SessionSnapshot> {
  const response = await fetch(apiUrl(bridgeUrl, "/api/sessions"), {
    method: "POST",
    headers: {
      Authorization: `Bearer ${sessionToken}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(messageBody(text, attachments)),
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Create session request failed with ${response.status}`);
  }

  return parseSessionSnapshot(await response.json());
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
  if (!isCanonicalAssetPath(src)) {
    return null;
  }

  const bridge = new URL(bridgeUrl);
  const url = new URL(src, bridge);
  if (url.origin !== bridge.origin || url.pathname !== src) {
    return null;
  }

  return url.toString();
}

function isCanonicalAssetPath(src: string): boolean {
  if (!src.startsWith("/api/assets/")) {
    return false;
  }
  if (src.startsWith("//") || /[a-zA-Z][a-zA-Z\d+.-]*:/.test(src)) {
    return false;
  }
  if (src.includes("\\") || src.includes("?") || src.includes("#") || src.includes("%")) {
    return false;
  }

  const segments = src.split("/");
  return segments.every((segment) => segment !== "." && segment !== "..");
}

export async function sendTextMessage(
  bridgeUrl: string,
  sessionToken: string,
  threadId: string,
  text: string,
  attachments: OutgoingImageAttachment[] = [],
  clientMessageId: string,
): Promise<void> {
  const url = apiUrl(bridgeUrl, `/api/sessions/${encodeURIComponent(threadId)}/messages`);
  const request = {
    method: "POST",
    headers: {
      Authorization: `Bearer ${sessionToken}`,
      "Content-Type": "application/json",
      "X-Codex-Client-Message-Id": clientMessageId,
    },
    body: JSON.stringify(messageBody(text, attachments)),
  };

  for (let attempt = 0; attempt <= SEND_MESSAGE_RETRY_DELAYS_MS.length; attempt += 1) {
    let response: Response;
    try {
      response = await fetch(url, request);
    } catch (error) {
      if (attempt === SEND_MESSAGE_RETRY_DELAYS_MS.length || !(error instanceof TypeError)) {
        throw error;
      }
      await waitForRetry(SEND_MESSAGE_RETRY_DELAYS_MS[attempt]);
      continue;
    }

    if (response.ok) {
      return;
    }

    const apiError = new ApiError(response.status, `Send message request failed with ${response.status}`);
    if (attempt === SEND_MESSAGE_RETRY_DELAYS_MS.length || !isTransientSendStatus(response.status)) {
      throw apiError;
    }
    await waitForRetry(SEND_MESSAGE_RETRY_DELAYS_MS[attempt]);
  }
}

const SEND_MESSAGE_RETRY_DELAYS_MS = [250, 750];

function isTransientSendStatus(status: number): boolean {
  return status === 408 || status === 429 || status >= 500;
}

function waitForRetry(delayMs: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, delayMs));
}

function messageBody(text: string, attachments: OutgoingImageAttachment[]): { text: string; attachments?: OutgoingImageAttachment[] } {
  if (attachments.length === 0) {
    return { text };
  }
  return { text, attachments };
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

function parseApprovalRequests(value: unknown): ApprovalRequest[] {
  if (!Array.isArray(value)) {
    throw new ApiValidationError("Approvals response must be an array");
  }
  return value.map((approval) => {
    if (!isApprovalRequest(approval)) {
      throw new ApiValidationError("Approval response is missing required fields");
    }
    return approval;
  });
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

function parseSessionEventPage(value: unknown): SessionEventPage {
  if (Array.isArray(value)) {
    const events = parseSessionEvents(value);
    return {
      events,
      beforeCursor: events[0]?.id,
      afterCursor: events.at(-1)?.id,
      hasMoreBefore: false,
      hasMoreAfter: false,
      reset: false,
      legacySnapshot: true,
    };
  }
  if (!value || typeof value !== "object") {
    throw new ApiValidationError("Session event page must be an object");
  }

  const page = value as Record<string, unknown>;
  if (
    !Array.isArray(page.events) ||
    typeof page.hasMoreBefore !== "boolean" ||
    typeof page.hasMoreAfter !== "boolean" ||
    typeof page.reset !== "boolean" ||
    !isOptionalString(page.beforeCursor) ||
    !isOptionalString(page.afterCursor)
  ) {
    throw new ApiValidationError("Session event page is missing required fields");
  }

  return {
    events: parseSessionEvents(page.events),
    beforeCursor: typeof page.beforeCursor === "string" ? page.beforeCursor : undefined,
    afterCursor: typeof page.afterCursor === "string" ? page.afterCursor : undefined,
    hasMoreBefore: page.hasMoreBefore,
    hasMoreAfter: page.hasMoreAfter,
    reset: page.reset,
    legacySnapshot: false,
  };
}

function isOptionalString(value: unknown): boolean {
  return value === undefined || value === null || typeof value === "string";
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
