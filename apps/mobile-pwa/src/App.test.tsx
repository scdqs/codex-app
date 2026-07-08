import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App, { appendOrMergeSessionEvent } from "./App";
import type { SessionEvent, SessionSnapshot } from "./protocol";
import { clearSession, loadSession, saveSession } from "./storage";

describe("App", () => {
  beforeEach(() => {
    MockWebSocket.instances = [];
    vi.stubGlobal("WebSocket", MockWebSocket);
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    clearSession();
    window.history.replaceState(null, "", "/");
  });

  it("renders the mobile workbench regions and selected session detail", async () => {
    const user = userEvent.setup();
    render(<App />);

    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Unpaired");
    expect(screen.getByRole("heading", { name: "Pending approvals" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Mobile bridge MVP" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Mobile bridge MVP")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject Run npm install" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve Run npm install" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject Create PWA scaffold" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve Create PWA scaffold" })).toBeInTheDocument();

    expect(screen.getByRole("button", { name: /Mobile bridge MVP/ })).toHaveAttribute("aria-current", "true");

    await user.click(screen.getByRole("button", { name: /Bridge sidecar API/ }));

    expect(screen.getByRole("heading", { name: "Bridge sidecar API" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Bridge sidecar API")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Bridge sidecar API/ })).toHaveAttribute("aria-current", "true");
  });

  it("shows_revoked_or_expired_connection_error", async () => {
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "expired-token",
      sessionExpiresAt: 1_767_225_600_000,
      bridgeUrl: "http://bridge.local",
    });
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(JSON.stringify({ message: "revoked" }), {
        status: 401,
        headers: { "Content-Type": "application/json" },
      }),
    );

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connection error");
    });
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Session revoked or expired");
  });

  it("hides_sample_data_when_pairing_link_is_invalid", async () => {
    window.history.replaceState(
      null,
      "",
      "/?pairingToken=used-token&bridgeUrl=http%3A%2F%2Fbridge.local",
    );
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(jsonResponse({ error: "invalid pairing token" }, 400));

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connection error");
    });
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Pairing link expired");
    expect(screen.queryByText("Run npm install")).not.toBeInTheDocument();
    expect(screen.queryByText("Mobile bridge MVP")).not.toBeInTheDocument();
    expect(screen.getByText("No live sessions yet. Use the newest pairing URL from the bridge terminal.")).toBeInTheDocument();
  });

  it("does_not_show_writable_when_desktop_injection_fails", async () => {
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "session-1",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(jsonResponse({ status: "degraded", connectionState: "inject_failed" }));

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Inject failed");
    });
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Desktop bridge unavailable");
    expect(screen.getByLabelText("Connection status")).not.toHaveTextContent("Writable");
    expect(screen.getByText("No live sessions yet. Use the newest pairing URL from the bridge terminal.")).toBeInTheDocument();
  });

  it("clears_pairing_token_after_successful_pairing", async () => {
    window.history.replaceState(
      null,
      "",
      "/?pairingToken=pair-1&bridgeUrl=http%3A%2F%2Fbridge.local&deviceName=Damon%20Phone&keep=1",
    );
    const replaceState = vi.spyOn(window.history, "replaceState");
    vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            deviceId: "device-1",
            sessionToken: "session-1",
            sessionExpiresAt: Date.now() + 60_000,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ status: "ok", connectionState: "connected" }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      );

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connected");
    });
    expect(replaceState).toHaveBeenCalledWith(null, "", "/?keep=1");
    expect(loadSession()?.sessionToken).toBe("session-1");
  });

  it("uses_saved_session_when_url_pairing_token_is_stale", async () => {
    window.history.replaceState(
      null,
      "",
      "/?pairingToken=stale-token&bridgeUrl=http%3A%2F%2Fstale.local&keep=1",
    );
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "session-1",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    const replaceState = vi.spyOn(window.history, "replaceState");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "connected" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connected");
    });
    expect(replaceState).toHaveBeenCalledWith(null, "", "/?keep=1");
    expect(globalThis.fetch).toHaveBeenCalledWith("http://bridge.local/api/health", {
      headers: { Authorization: "Bearer session-1" },
    });
    expect(
      vi
        .mocked(globalThis.fetch)
        .mock.calls.some(([input]) => String(input) === "http://stale.local/api/pairing/complete"),
    ).toBe(false);
  });

  it("refreshes_expired_saved_session_instead_of_reusing_stale_pairing_token", async () => {
    window.history.replaceState(
      null,
      "",
      "/?pairingToken=stale-token&bridgeUrl=http%3A%2F%2Fstale.local&keep=1",
    );
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() - 60_000,
      bridgeUrl: "http://bridge.local",
    });
    const replaceState = vi.spyOn(window.history, "replaceState");
    vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            deviceId: "device-1",
            sessionToken: "new-token",
            sessionExpiresAt: Date.now() + 120_000,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ status: "ok", connectionState: "connected" }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      );

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connected");
    });
    expect(replaceState).toHaveBeenCalledWith(null, "", "/?keep=1");
    expect(loadSession()?.sessionToken).toBe("new-token");
    expect(globalThis.fetch).toHaveBeenCalledTimes(3);
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "http://bridge.local/api/session/refresh",
      expect.objectContaining({ method: "POST" }),
    );
    expect(
      vi
        .mocked(globalThis.fetch)
        .mock.calls.some(([input]) => String(input) === "http://bridge.local/api/pairing/complete"),
    ).toBe(false);
  });

  it("shares_in_flight_pairing_request_under_strict_mode", async () => {
    window.history.replaceState(null, "", "/?pairingToken=pair-1&bridgeUrl=http%3A%2F%2Fbridge.local");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/pairing/complete" && init?.method === "POST") {
        return new Response(
          JSON.stringify({
            deviceId: "device-1",
            sessionToken: "session-1",
            sessionExpiresAt: Date.now() + 60_000,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }

      return new Response(JSON.stringify({ status: "ok", connectionState: "connected" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    });

    render(
      <StrictMode>
        <App />
      </StrictMode>,
    );

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connected");
    });
    const pairingCalls = vi
      .mocked(globalThis.fetch)
      .mock.calls.filter(([input, init]) => String(input) === "http://bridge.local/api/pairing/complete" && init?.method === "POST");
    expect(pairingCalls).toHaveLength(1);
  });

  it("refreshes_session_once_when_health_rejects_token", async () => {
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(new Response(JSON.stringify({ message: "expired" }), { status: 401 }))
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            deviceId: "device-1",
            sessionToken: "new-token",
            sessionExpiresAt: Date.now() + 120_000,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ status: "ok", connectionState: "writable" }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      );

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");
    });
    expect(loadSession()?.sessionToken).toBe("new-token");
    const refreshCalls = vi
      .mocked(globalThis.fetch)
      .mock.calls.filter(([input, init]) => String(input) === "http://bridge.local/api/session/refresh" && init?.method === "POST");
    expect(refreshCalls).toHaveLength(1);
  });

  it("rejects_malformed_pairing_response_without_saving_session", async () => {
    window.history.replaceState(null, "", "/?pairingToken=pair-1&bridgeUrl=http%3A%2F%2Fbridge.local");
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(JSON.stringify({ deviceId: "device-1" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connection error");
    });
    expect(loadSession()).toBeNull();
  });

  it("renders_session_list_and_selects_thread", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-a", title: "Implement bridge UI", preview: "First thread" }),
          sessionSnapshot({ threadId: "thread-b", title: "Review sidecar API", preview: "Second thread" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-a/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-a",
            threadId: "thread-a",
            payload: { role: "assistant", text: "Loaded first thread." },
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-b/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-b",
            threadId: "thread-b",
            payload: { role: "assistant", text: "Loaded second thread." },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getByRole("button", { name: /Implement bridge UI/ })).toBeInTheDocument();
    });
    expect(await screen.findByText("Loaded first thread.")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /Review sidecar API/ }));

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Review sidecar API" })).toBeInTheDocument();
    });
    expect(screen.getByText("Loaded second thread.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Review sidecar API/ })).toHaveAttribute("aria-current", "true");
  });

  it("merges_message_delta_into_current_assistant_message", () => {
    const base: SessionEvent[] = [
      sessionEvent({
        id: "assistant-1",
        threadId: "thread-a",
        payload: { role: "assistant", text: "Hel" },
      }),
    ];

    const merged = appendOrMergeSessionEvent(
      appendOrMergeSessionEvent(
        base,
        sessionEvent({
          id: "delta-1",
          threadId: "thread-a",
          type: "message_delta",
          payload: { delta: "lo" },
        }),
      ),
      sessionEvent({
        id: "delta-2",
        threadId: "thread-a",
        type: "message_delta",
        payload: { text: "!" },
      }),
    );

    expect(merged).toHaveLength(1);
    expect(merged[0].payload).toEqual({ role: "assistant", text: "Hello!" });
  });

  it("starts_new_delta_tail_after_intervening_user_message", () => {
    const merged = appendOrMergeSessionEvent(
      [
        sessionEvent({
          id: "assistant-1",
          threadId: "thread-a",
          payload: { role: "assistant", text: "Old assistant" },
        }),
        sessionEvent({
          id: "user-1",
          threadId: "thread-a",
          payload: { role: "user", text: "New prompt" },
        }),
      ],
      sessionEvent({
        id: "delta-1",
        threadId: "thread-a",
        type: "message_delta",
        payload: { delta: "Fresh answer" },
      }),
    );

    expect(merged).toHaveLength(3);
    expect(merged[0].payload).toEqual({ role: "assistant", text: "Old assistant" });
    expect(merged[2].payload).toEqual({ role: "assistant", text: "Fresh answer" });
  });

  it("send_text_posts_to_selected_thread", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-send", title: "Reply target", preview: "Waiting" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-send/events") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/sessions/thread-send/messages" && init?.method === "POST") {
        return jsonResponse({ accepted: true });
      }
      return jsonResponse({});
    });

    render(<App />);

    const input = await screen.findByPlaceholderText("Message Reply target");
    await user.type(input, "continue from phone");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => {
      expect(globalThis.fetch).toHaveBeenCalledWith(
        "http://bridge.local/api/sessions/thread-send/messages",
        expect.objectContaining({
          method: "POST",
          headers: {
            Authorization: "Bearer session-1",
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ text: "continue from phone" }),
        }),
      );
    });
    expect(input).toHaveValue("");
  });

  it("renders_empty_state_and_disables_composer_when_sessions_are_empty", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("No sessions available")).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message No session selected")).toBeDisabled();
    expect(screen.getByRole("button", { name: "Send message" })).toBeDisabled();
    expect(
      vi
        .mocked(globalThis.fetch)
        .mock.calls.some(([input]) => String(input).includes("/events")),
    ).toBe(false);
  });

  it("hides_sample_approvals_after_live_sessions_loads_until_real_approval_arrives", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-live", title: "Live thread", preview: "Real session" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-live/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByRole("heading", { name: "Live thread" });
    await waitFor(() => {
      expect(screen.queryByText("Run npm install")).not.toBeInTheDocument();
    });

    act(() => {
      MockWebSocket.instances[0].emit({
        type: "approval_request",
        payload: {
          id: "approval-real",
          threadId: "thread-live",
          kind: "command",
          title: "Run real check",
          detail: "npm test",
          createdAt: 1_783_515_390_000,
        },
      });
    });

    expect(await screen.findByText("Run real check")).toBeInTheDocument();
  });

  it("preserves_newer_ws_event_when_http_events_resolve_later", async () => {
    saveActiveSession();
    let resolveEvents: (response: Response) => void = () => {};
    const eventsResponse = new Promise<Response>((resolve) => {
      resolveEvents = resolve;
    });
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-live", title: "Live thread", preview: "Real session" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-live/events") {
        return eventsResponse;
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByRole("heading", { name: "Live thread" });
    act(() => {
      MockWebSocket.instances[0].emit({
        type: "session_event",
        payload: sessionEvent({
          id: "event-ws",
          threadId: "thread-live",
          payload: { role: "assistant", text: "Arrived over socket" },
        }),
      });
    });
    expect(await screen.findByText("Arrived over socket")).toBeInTheDocument();

    await act(async () => {
      resolveEvents(
        jsonResponse([
          sessionEvent({
            id: "event-http",
            threadId: "thread-live",
            payload: { role: "assistant", text: "Loaded over HTTP" },
          }),
        ]),
      );
    });

    expect(await screen.findByText("Loaded over HTTP")).toBeInTheDocument();
    expect(screen.getByText("Arrived over socket")).toBeInTheDocument();
  });

  it("reconciles_optimistic_user_message_with_matching_server_echo", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-send", title: "Reply target", preview: "Waiting" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-send/events") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/sessions/thread-send/messages" && init?.method === "POST") {
        return jsonResponse({ accepted: true });
      }
      return jsonResponse({});
    });

    render(<App />);

    const input = await screen.findByPlaceholderText("Message Reply target");
    await user.type(input, "same text");
    await user.click(screen.getByRole("button", { name: "Send message" }));
    expect(await screen.findByText("same text")).toBeInTheDocument();

    act(() => {
      MockWebSocket.instances[0].emit({
        type: "session_event",
        payload: sessionEvent({
          id: "event-echo",
          threadId: "thread-send",
          payload: { role: "user", text: "same text" },
        }),
      });
    });

    await waitFor(() => {
      expect(screen.getAllByText("same text")).toHaveLength(1);
    });
  });

  it("ignores_malformed_ws_event_and_handles_valid_approval_request", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-live", title: "Live thread", preview: "Real session" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-live/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByRole("heading", { name: "Live thread" });
    act(() => {
      MockWebSocket.instances[0].emit({
        type: "session_event",
        payload: { id: "bad-event", threadId: "thread-live", type: "message" },
      });
      MockWebSocket.instances[0].emit({
        type: "approval_request",
        payload: {
          id: "approval-valid",
          threadId: "thread-live",
          kind: "command",
          title: "Approve valid command",
          detail: "echo ok",
          createdAt: 1_783_515_390_000,
        },
      });
    });

    expect(screen.queryByText("bad-event")).not.toBeInTheDocument();
    expect(await screen.findByText("Approve valid command")).toBeInTheDocument();
  });

  it("renders_approval_card_and_approve_reject_buttons", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-live",
            title: "Live thread",
            preview: "Real session",
            status: "waiting_for_approval",
            pendingApprovalIds: ["approval-real"],
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-live/events") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/approvals/approval-real/decision" && init?.method === "POST") {
        return jsonResponse({ accepted: true }, 202);
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByRole("heading", { name: "Live thread" });
    act(() => {
      MockWebSocket.instances[0].emit({
        type: "approval_request",
        payload: {
          id: "approval-real",
          threadId: "thread-live",
          kind: "command",
          title: "Run real check",
          detail: "npm test",
          riskHint: "Runs local tests",
          createdAt: 1_783_515_390_000,
        },
      });
    });

    expect(await screen.findByText("Run real check")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject Run real check" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Approve Run real check" }));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        "http://bridge.local/api/approvals/approval-real/decision",
        expect.objectContaining({
          method: "POST",
          headers: expect.objectContaining({
            Authorization: "Bearer session-1",
            "Content-Type": "application/json",
          }),
          body: JSON.stringify({ decision: "approve" }),
        }),
      );
    });
    await waitFor(() => {
      expect(screen.queryByText("Run real check")).not.toBeInTheDocument();
    });
    expect(await screen.findByText("approval resolved")).toBeInTheDocument();
  });
});

function saveActiveSession() {
  saveSession({
    deviceId: "device-1",
    deviceSecret: "secret-1",
    displayName: "Damon Phone",
    sessionToken: "session-1",
    sessionExpiresAt: Date.now() + 60_000,
    bridgeUrl: "http://bridge.local",
  });
}

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function sessionSnapshot(overrides: Partial<SessionSnapshot> = {}): SessionSnapshot {
  return {
    threadId: "thread-a",
    title: "Thread A",
    preview: "Preview",
    updatedAt: 1_783_515_380_000,
    status: "idle",
    pendingApprovalIds: [],
    ...overrides,
  };
}

function sessionEvent(overrides: Partial<SessionEvent> = {}): SessionEvent {
  return {
    id: "event-1",
    threadId: "thread-a",
    type: "message",
    payload: { role: "assistant", text: "Hello" },
    createdAt: 1_783_515_380_000,
    ...overrides,
  };
}

class MockWebSocket {
  static instances: MockWebSocket[] = [];
  onmessage: ((message: MessageEvent) => void) | null = null;
  closed = false;

  constructor(readonly url: string) {
    MockWebSocket.instances.push(this);
  }

  emit(envelope: unknown) {
    this.onmessage?.({ data: JSON.stringify(envelope) } as MessageEvent);
  }

  close() {
    this.closed = true;
  }
}
