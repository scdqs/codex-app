export type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

export type SessionStatus =
  | "idle"
  | "running"
  | "waiting_for_input"
  | "waiting_for_approval"
  | "error";

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

export type SessionEventType =
  | "message"
  | "message_delta"
  | "tool_call"
  | "tool_result"
  | "approval_requested"
  | "approval_resolved"
  | "status_changed"
  | "error";

export interface SessionEvent {
  id: string;
  threadId: string;
  type: SessionEventType;
  payload: JsonValue;
  createdAt: number;
}

export type ApprovalKind = "command" | "file_edit" | "network" | "mcp" | "unknown";

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

export type DecisionKind = "approve" | "reject";

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
