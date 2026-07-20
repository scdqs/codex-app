import type { JsonValue, SessionEvent } from "@codex/bridge-protocol";

export interface StandaloneEventDisplayGroup {
  kind: "event";
  key: string;
  event: SessionEvent;
}

export interface AssistantTurnDisplayGroup {
  kind: "assistant_turn";
  key: string;
  turnScope: string;
  events: SessionEvent[];
}

export type SessionEventDisplayGroup = StandaloneEventDisplayGroup | AssistantTurnDisplayGroup;

export function groupSessionEventsForDisplay(events: SessionEvent[]): SessionEventDisplayGroup[] {
  const groups: SessionEventDisplayGroup[] = [];
  const turnOccurrences = new Map<string, number>();

  for (const event of events) {
    if (event.type === "status_changed" || isEmptyDisplayEvent(event)) {
      continue;
    }

    const turnScope = isAssistantTurnEvent(event) ? eventTurnScope(event) : null;
    if (!turnScope) {
      groups.push({ kind: "event", key: `event:${event.id}`, event });
      continue;
    }

    const previousGroup = groups.at(-1);
    if (previousGroup?.kind === "assistant_turn" && previousGroup.turnScope === turnScope) {
      previousGroup.events.push(event);
      continue;
    }

    const occurrence: number = turnOccurrences.get(turnScope) ?? 0;
    turnOccurrences.set(turnScope, occurrence + 1);
    const nextGroup: AssistantTurnDisplayGroup = {
      kind: "assistant_turn",
      key: `assistant-turn:${turnScope}:${occurrence}`,
      turnScope,
      events: [event],
    };
    groups.push(nextGroup);
  }

  return groups;
}

function isAssistantTurnEvent(event: SessionEvent): boolean {
  if (event.type === "message" || event.type === "message_delta") {
    return payloadRole(event.payload) === "assistant";
  }
  return (
    event.type === "reasoning_summary" ||
    event.type === "reasoning_summary_delta" ||
    event.type === "plan" ||
    event.type === "plan_delta" ||
    event.type === "tool_call" ||
    event.type === "tool_result"
  );
}

function isEmptyDisplayEvent(event: SessionEvent): boolean {
  const textIsEmpty = payloadText(event.payload).trim().length === 0;
  if (event.type === "tool_result") {
    return textIsEmpty;
  }
  if (
    event.type === "reasoning_summary" ||
    event.type === "reasoning_summary_delta" ||
    event.type === "plan" ||
    event.type === "plan_delta"
  ) {
    return textIsEmpty;
  }
  if (event.type === "message" || event.type === "message_delta") {
    return payloadRole(event.payload) === "assistant" && textIsEmpty && !hasAttachments(event.payload);
  }
  return false;
}

export function eventTurnScope(event: SessionEvent): string | null {
  const payload = recordValue(event.payload);
  const raw = recordValue(payload?.raw);
  const explicitTurnId = stringValue(payload?.turnId) ??
    stringValue(payload?.turn_id) ??
    stringValue(raw?.turnId) ??
    stringValue(raw?.turn_id);
  if (explicitTurnId) {
    return explicitTurnId;
  }

  const fallbackScope = /^(.*:turn-\d+):(?:item-\d+|\d+)(?::.*)?$/.exec(event.id)?.[1];
  if (fallbackScope) {
    return fallbackScope;
  }

  const separator = event.id.indexOf(":");
  return separator > 0 ? event.id.slice(0, separator) : null;
}

function hasAttachments(payload: JsonValue): boolean {
  const attachments = recordValue(payload)?.attachments;
  return Array.isArray(attachments) && attachments.length > 0;
}

function payloadRole(payload: JsonValue): string | null {
  return stringValue(recordValue(payload)?.role);
}

function payloadText(payload: JsonValue): string {
  const text = recordValue(payload)?.text;
  return typeof text === "string" ? text : "";
}

function recordValue(value: JsonValue | undefined): Record<string, JsonValue> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value
    : null;
}

function stringValue(value: JsonValue | undefined): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}
