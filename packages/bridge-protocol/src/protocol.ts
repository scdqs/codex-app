export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export const SESSION_STATUSES = [
  "idle",
  "running",
  "waiting_for_input",
  "waiting_for_approval",
  "error",
] as const;

export type SessionStatus = (typeof SESSION_STATUSES)[number];

export interface SessionSnapshot {
  threadId: string;
  title: string;
  cwd?: string;
  modelProvider?: string;
  preview?: string;
  updatedAt: number;
  status: SessionStatus;
  pendingApprovalIds: string[];
}

export interface WorkspaceOption {
  cwd: string;
}

export const API_ERROR_CODES = [
  "unauthorized",
  "invalid_request",
  "forbidden",
  "not_found",
  "unsupported_media_type",
  "internal_error",
  "invalid_pairing_token",
  "expired_pairing_token",
  "device_revoked",
  "device_not_found",
  "adapter_error",
  "workspace_required",
  "workspace_not_allowed",
  "workspace_unavailable",
] as const;

export type ApiErrorCode = (typeof API_ERROR_CODES)[number];

export const SESSION_EVENT_TYPES = [
  "message",
  "message_delta",
  "tool_call",
  "tool_result",
  "approval_requested",
  "approval_resolved",
  "status_changed",
  "error",
] as const;

export type SessionEventType = (typeof SESSION_EVENT_TYPES)[number];

export interface SessionEvent {
  id: string;
  threadId: string;
  type: SessionEventType;
  payload: JsonValue;
  createdAt: number;
}

export interface ImageAttachment {
  type: "image";
  src: string;
  name: string;
}

export interface MessagePayload {
  role?: string;
  text?: string;
  pending?: boolean;
  attachments?: ImageAttachment[];
}

export const APPROVAL_KINDS = ["command", "file_edit", "network", "mcp", "unknown"] as const;

export type ApprovalKind = (typeof APPROVAL_KINDS)[number];

export interface ApprovalRequest {
  id: string;
  threadId: string;
  kind: ApprovalKind;
  title: string;
  detail: string;
  riskHint?: string;
  raw?: JsonValue;
  createdAt: number;
  expiresAt?: number;
}

export const DECISION_KINDS = ["approve", "reject"] as const;

export type DecisionKind = (typeof DECISION_KINDS)[number];

export interface ApprovalDecision {
  approvalId: string;
  decision: DecisionKind;
  comment?: string;
  deviceId: string;
  decidedAt: number;
}

export type ServerEnvelope =
  | { type: "session_snapshot"; payload: SessionSnapshot }
  | { type: "session_event"; payload: SessionEvent }
  | { type: "approval_request"; payload: ApprovalRequest }
  | { type: "approval_resolved"; payload: ApprovalDecision }
  | { type: "error"; payload: { message: string } };

export type ClientCommand =
  | { type: "subscribe"; payload: { threadId?: string } }
  | { type: "send_message"; payload: { threadId: string; text: string } }
  | { type: "approval_decision"; payload: ApprovalDecision };

export function parseServerEnvelope(data: unknown): ServerEnvelope | null {
  if (typeof data !== "string") {
    return null;
  }
  try {
    const value = JSON.parse(data) as unknown;
    return isServerEnvelope(value) ? value : null;
  } catch {
    return null;
  }
}

export function isServerEnvelope(value: unknown): value is ServerEnvelope {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const envelope = value as Record<string, unknown>;
  switch (envelope.type) {
    case "session_snapshot":
      return isSessionSnapshot(envelope.payload);
    case "session_event":
      return isSessionEvent(envelope.payload);
    case "approval_request":
      return isApprovalRequest(envelope.payload);
    case "approval_resolved":
      return isApprovalDecision(envelope.payload);
    case "error":
      return isErrorPayload(envelope.payload);
    default:
      return false;
  }
}

export function isSessionSnapshot(value: unknown): value is SessionSnapshot {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const session = value as Record<string, unknown>;
  return (
    typeof session.threadId === "string" &&
    typeof session.title === "string" &&
    (session.cwd === undefined || typeof session.cwd === "string") &&
    (session.modelProvider === undefined || typeof session.modelProvider === "string") &&
    (session.preview === undefined || typeof session.preview === "string") &&
    typeof session.updatedAt === "number" &&
    Number.isFinite(session.updatedAt) &&
    isSessionStatus(session.status) &&
    Array.isArray(session.pendingApprovalIds) &&
    session.pendingApprovalIds.every((id) => typeof id === "string")
  );
}

export function isWorkspaceOption(value: unknown): value is WorkspaceOption {
  return Boolean(
    value &&
      typeof value === "object" &&
      !Array.isArray(value) &&
      typeof (value as Record<string, unknown>).cwd === "string",
  );
}

export function isApiErrorCode(value: unknown): value is ApiErrorCode {
  return typeof value === "string" && API_ERROR_CODES.includes(value as ApiErrorCode);
}

export function isSessionEvent(value: unknown): value is SessionEvent {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const event = value as Record<string, unknown>;
  return (
    typeof event.id === "string" &&
    typeof event.threadId === "string" &&
    isSessionEventType(event.type) &&
    isJsonValue(event.payload) &&
    typeof event.createdAt === "number" &&
    Number.isFinite(event.createdAt)
  );
}

export function isApprovalRequest(value: unknown): value is ApprovalRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const approval = value as Record<string, unknown>;
  return (
    typeof approval.id === "string" &&
    typeof approval.threadId === "string" &&
    isApprovalKind(approval.kind) &&
    typeof approval.title === "string" &&
    typeof approval.detail === "string" &&
    (approval.riskHint === undefined || typeof approval.riskHint === "string") &&
    (approval.raw === undefined || isJsonValue(approval.raw)) &&
    typeof approval.createdAt === "number" &&
    Number.isFinite(approval.createdAt) &&
    (approval.expiresAt === undefined ||
      (typeof approval.expiresAt === "number" && Number.isFinite(approval.expiresAt)))
  );
}

export function isApprovalDecision(value: unknown): value is ApprovalDecision {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const decision = value as Record<string, unknown>;
  return (
    typeof decision.approvalId === "string" &&
    isDecisionKind(decision.decision) &&
    (decision.comment === undefined || typeof decision.comment === "string") &&
    typeof decision.deviceId === "string" &&
    typeof decision.decidedAt === "number" &&
    Number.isFinite(decision.decidedAt)
  );
}

export function isErrorPayload(value: unknown): value is { message: string } {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    typeof (value as Record<string, unknown>).message === "string"
  );
}

export function isSessionStatus(value: unknown): value is SessionStatus {
  return includesLiteral(SESSION_STATUSES, value);
}

export function isSessionEventType(value: unknown): value is SessionEventType {
  return includesLiteral(SESSION_EVENT_TYPES, value);
}

export function isApprovalKind(value: unknown): value is ApprovalKind {
  return includesLiteral(APPROVAL_KINDS, value);
}

export function isDecisionKind(value: unknown): value is DecisionKind {
  return includesLiteral(DECISION_KINDS, value);
}

export function isJsonValue(value: unknown): value is JsonValue {
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

function includesLiteral<const T extends readonly string[]>(items: T, value: unknown): value is T[number] {
  return typeof value === "string" && (items as readonly string[]).includes(value);
}
