import { describe, expect, it } from "vitest";
import type { SessionEvent } from "@codex/bridge-protocol";
import { groupSessionEventsForDisplay } from "./turn-groups";

function event(overrides: Partial<SessionEvent>): SessionEvent {
  return {
    id: "turn-1:item-1",
    threadId: "thread-1",
    type: "message",
    payload: { role: "assistant", text: "Done" },
    createdAt: 1_725_000_000_000,
    ...overrides,
  };
}

describe("groupSessionEventsForDisplay", () => {
  it("keeps the prompt separate and groups one turn into one Codex response", () => {
    const groups = groupSessionEventsForDisplay([
      event({
        id: "turn-1:user-1",
        payload: { role: "user", text: "Please continue" },
      }),
      event({
        id: "turn-1:reasoning-1",
        type: "reasoning_summary",
        payload: { role: "reasoning", text: "Checking the implementation" },
      }),
      event({
        id: "turn-1:tool-result-empty",
        type: "tool_result",
        payload: { role: "tool_result", text: "" },
      }),
      event({
        id: "turn-1:plan-1",
        type: "plan",
        payload: { role: "plan", text: "Run the regression" },
      }),
      event({
        id: "turn-1:message-1",
        payload: { role: "assistant", text: "The change is ready." },
      }),
    ]);

    expect(groups).toHaveLength(2);
    expect(groups[0]).toMatchObject({ kind: "event", event: { id: "turn-1:user-1" } });
    expect(groups[1]).toMatchObject({
      kind: "assistant_turn",
      turnScope: "turn-1",
      events: [
        { id: "turn-1:reasoning-1" },
        { id: "turn-1:plan-1" },
        { id: "turn-1:message-1" },
      ],
    });
  });

  it("does not merge replies from different turns", () => {
    const groups = groupSessionEventsForDisplay([
      event({ id: "turn-1:message-1", payload: { role: "assistant", text: "First" } }),
      event({ id: "turn-2:message-1", payload: { role: "assistant", text: "Second" } }),
    ]);

    expect(groups.map((group) => group.key)).toEqual([
      "assistant-turn:turn-1:0",
      "assistant-turn:turn-2:0",
    ]);
  });

  it("hides_status_transition_events_from_the_conversation_stream", () => {
    const groups = groupSessionEventsForDisplay([
      event({
        id: "thread-1:turn-1:item:turn-started",
        type: "status_changed",
        payload: { status: "running" },
      }),
      event({
        id: "thread-1:turn-1:item:status",
        type: "status_changed",
        payload: { status: "running" },
      }),
      event({
        id: "turn-1:message-1",
        payload: { role: "assistant", text: "Finished" },
      }),
    ]);

    expect(groups).toHaveLength(1);
    expect(groups[0]).toMatchObject({
      kind: "assistant_turn",
      events: [{ id: "turn-1:message-1" }],
    });
  });

  it("hides_empty_streaming_cards", () => {
    const groups = groupSessionEventsForDisplay([
      event({
        id: "turn-1:reasoning-empty",
        type: "reasoning_summary_delta",
        payload: { role: "reasoning", text: "" },
      }),
      event({
        id: "turn-1:plan-empty",
        type: "plan_delta",
        payload: { role: "plan", text: "   " },
      }),
      event({
        id: "turn-1:message-empty",
        type: "message_delta",
        payload: { role: "assistant", text: "" },
      }),
    ]);

    expect(groups).toEqual([]);
  });
});
