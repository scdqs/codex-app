import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App, { appendOrMergeSessionEvent, mergePolledSessionEvents } from "./App";
import type { SessionEvent, SessionSnapshot } from "./protocol";
import { clearSession, loadSession, saveSession } from "./storage";

const originalScrollTo = (HTMLElement.prototype as Partial<HTMLElement>).scrollTo;
const originalCreateObjectURL = (URL as Partial<typeof URL>).createObjectURL;
const originalRevokeObjectURL = (URL as Partial<typeof URL>).revokeObjectURL;

describe("App", () => {
  beforeEach(() => {
    MockWebSocket.instances = [];
    vi.stubGlobal("WebSocket", MockWebSocket);
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.useRealTimers();
    vi.unstubAllGlobals();
    clearSession();
    window.history.replaceState(null, "", "/");
    restoreScrollTo();
    restoreObjectUrls();
  });

  it("renders the mobile workbench regions and selected session detail", () => {
    render(<App />);

    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Unpaired");
    expect(screen.getByRole("button", { name: "Open sessions" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Pending approvals" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Sessions" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Mobile bridge MVP" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Mobile bridge MVP")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject Run npm install" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve Run npm install" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject Create PWA scaffold" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve Create PWA scaffold" })).toBeInTheDocument();

    expect(screen.getByRole("button", { name: /Mobile bridge MVP/ })).toHaveAttribute("aria-current", "true");

    fireEvent.click(screen.getByRole("button", { name: /Bridge sidecar API/ }));

    expect(screen.getByRole("heading", { name: "Bridge sidecar API" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Bridge sidecar API")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Bridge sidecar API/ })).toHaveAttribute("aria-current", "true");
  });

  it("opens_and_closes_the_mobile_session_drawer", async () => {
    const user = userEvent.setup();

    render(<App />);

    const openButton = screen.getByRole("button", { name: "Open sessions" });
    await user.click(openButton);

    expect(screen.getByRole("dialog", { name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close sessions" })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Close sessions" }));

    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
    expect(openButton).toHaveFocus();
  });

  it("closes_the_mobile_session_drawer_from_the_backdrop", async () => {
    const user = userEvent.setup();

    render(<App />);

    await user.click(screen.getByRole("button", { name: "Open sessions" }));
    await user.click(screen.getByLabelText("Close sessions drawer"));

    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
  });

  it("selects_a_session_from_the_mobile_drawer_and_closes_it", async () => {
    const user = userEvent.setup();

    render(<App />);

    await user.click(screen.getByRole("button", { name: "Open sessions" }));
    const drawer = screen.getByRole("dialog", { name: "Sessions" });
    await user.click(within(drawer).getByRole("button", { name: /Bridge sidecar API/ }));

    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Bridge sidecar API" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Bridge sidecar API")).toBeInTheDocument();
  });

  it("keeps_focus_on_drawer_session_row_during_live_session_updates", async () => {
    const user = userEvent.setup();
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
    await user.click(screen.getByRole("button", { name: "Open sessions" }));
    const drawer = screen.getByRole("dialog", { name: "Sessions" });
    const row = within(drawer).getByRole("button", { name: /Live thread/ });
    row.focus();

    expect(document.activeElement).toBe(row);

    act(() => {
      MockWebSocket.instances[0].emit({
        type: "session_snapshot",
        payload: sessionSnapshot({
          threadId: "thread-live",
          title: "Live thread",
          preview: "Updated while drawer stays open",
          updatedAt: 1_783_515_390_000,
        }),
      });
    });

    expect(screen.getByRole("dialog", { name: "Sessions" })).toBeInTheDocument();
    expect(document.activeElement).toBe(row);
    expect(screen.getByRole("button", { name: "Close sessions" })).not.toHaveFocus();
  });

  it("closes_the_mobile_session_drawer_with_escape", async () => {
    const user = userEvent.setup();

    render(<App />);

    const openButton = screen.getByRole("button", { name: "Open sessions" });
    await user.click(openButton);
    await user.keyboard("{Escape}");

    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
    expect(openButton).toHaveFocus();
  });

  it("uses_independent_scroll_containers_for_sessions_and_events", () => {
    const stylesUrl = new URL("./styles.css", import.meta.url);
    const stylesPath =
      stylesUrl.protocol === "file:"
        ? stylesUrl
        : stylesUrl.pathname.startsWith("/@fs/")
          ? stylesUrl.pathname.slice("/@fs".length)
          : `.${stylesUrl.pathname}`;
    const css = readFileSync(stylesPath, "utf8");

    expect(css).toContain(".workbench");
    expect(css).toContain("overflow: hidden");
    expect(css).toContain(".session-list,");
    expect(css).toContain(".event-stream");
    expect(css).toContain("overflow-y: auto");
    expect(css).toContain(".session-list-panel,");
    expect(css).toContain("flex-direction: column");
    expect(css).toContain(".desktop-session-panel");
    expect(css).toContain(".session-drawer-layer");
    expect(css).toContain("grid-template-columns: minmax(0, 0.95fr) minmax(0, 1.45fr)");
    expect(css).toContain("grid-template-rows: minmax(0, 1fr)");
    expect(css).not.toContain("grid-template-rows: minmax(150px, 0.42fr) minmax(0, 1fr)");
    expect(css).toMatch(/\.session-list\s*\{[^}]*align-content:\s*start;/);
    expect(css).toMatch(/\.event-stream\s*\{[^}]*align-content:\s*start;/);
  });

  it("defines_mobile_session_drawer_layout_without_a_stacked_session_panel", () => {
    const stylesUrl = new URL("./styles.css", import.meta.url);
    const stylesPath =
      stylesUrl.protocol === "file:"
        ? stylesUrl
        : stylesUrl.pathname.startsWith("/@fs/")
          ? stylesUrl.pathname.slice("/@fs".length)
          : `.${stylesUrl.pathname}`;
    const css = readFileSync(stylesPath, "utf8");

    expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.connection-bar\s*\{[\s\S]*grid-template-columns:\s*38px minmax\(0, 1fr\) minmax\(0, auto\);/);
    expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.session-menu-button\s*\{[\s\S]*display:\s*grid;/);
    expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.desktop-session-panel\s*\{[\s\S]*display:\s*none;/);
    expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.session-drawer\s*\{[\s\S]*width:\s*min\(84vw, 340px\);/);
    expect(css).not.toContain("grid-template-rows: minmax(150px, 0.42fr) minmax(0, 1fr)");
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

  it("uses_new_pairing_link_when_saved_session_is_revoked", async () => {
    window.history.replaceState(
      null,
      "",
      "/?pairingToken=pair-2&bridgeUrl=http%3A%2F%2Fbridge.local&keep=1",
    );
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "revoked-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    const replaceState = vi.spyOn(window.history, "replaceState");
    vi.spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            deviceId: "device-1",
            sessionToken: "new-session",
            sessionExpiresAt: Date.now() + 60_000,
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      )
      .mockResolvedValueOnce(jsonResponse({ status: "ok", connectionState: "writable" }));

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");
    });
    expect(replaceState).toHaveBeenCalledWith(null, "", "/?keep=1");
    expect(loadSession()?.sessionToken).toBe("new-session");
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "http://bridge.local/api/pairing/complete",
      expect.objectContaining({ method: "POST" }),
    );
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
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://stale.local/api/pairing/complete" && init?.method === "POST") {
        return jsonResponse({ error: "invalid pairing token" }, 400);
      }
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
        .mock.calls.some(([input, init]) => String(input) === "http://stale.local/api/pairing/complete" && init?.method === "POST"),
    ).toBe(true);
  });

  it("refreshes_expired_saved_session_after_stale_pairing_token_fails", async () => {
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
      .mockResolvedValueOnce(jsonResponse({ error: "invalid pairing token" }, 400))
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
    expect(globalThis.fetch).toHaveBeenCalledTimes(4);
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "http://stale.local/api/pairing/complete",
      expect.objectContaining({ method: "POST" }),
    );
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "http://bridge.local/api/session/refresh",
      expect.objectContaining({ method: "POST" }),
    );
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

  it("renders_message_markdown_with_codex_like_structure", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-markdown", title: "Markdown thread", preview: "Formatted" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-markdown/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-markdown",
            threadId: "thread-markdown",
            payload: {
              role: "assistant",
              text: [
                "现在状态：",
                "",
                "- sidecar 已重启，`57324` 正在运行",
                "- `/api/health` 是 `ok / writable`",
                "",
                "手机用这条新链接：",
                "",
                "[打开 Codex Mobile](http://192.168.1.166:57324/)",
                "",
                "```bash",
                "npm run build",
                "```",
              ].join("\n"),
            },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await waitFor(() => {
      expect(document.querySelectorAll(".message-body li")).toHaveLength(2);
    });
    const listItems = Array.from(document.querySelectorAll(".message-body li")).map((item) => item.textContent);
    expect(listItems).toEqual([
      "sidecar 已重启，57324 正在运行",
      "/api/health 是 ok / writable",
    ]);
    expect(screen.getByRole("link", { name: "打开 Codex Mobile" })).toHaveAttribute(
      "href",
      "http://192.168.1.166:57324/",
    );
    expect(screen.getByText("npm run build")).toBeInTheDocument();
    expect(document.querySelectorAll(".message-body code")).toHaveLength(4);
  });

  it("distinguishes_user_and_codex_message_rows", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-roles", title: "Role thread", preview: "Conversation" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-roles/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-user",
            threadId: "thread-roles",
            payload: { role: "user", text: "Can you check this?" },
          }),
          sessionEvent({
            id: "event-assistant",
            threadId: "thread-roles",
            payload: { role: "assistant", text: "I checked it." },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("Can you check this?")).toBeInTheDocument();
    expect(await screen.findByText("I checked it.")).toBeInTheDocument();

    const rows = Array.from(document.querySelectorAll(".event-row"));
    expect(rows[0]).toHaveClass("user");
    expect(rows[0]).toHaveTextContent("You");
    expect(rows[1]).toHaveClass("assistant");
    expect(rows[1]).toHaveTextContent("Codex");
  });

  it("renders_image_attachments_with_authenticated_asset_fetch", async () => {
    stubObjectUrls();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-image", title: "Image thread", preview: "Attachment" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-image/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-image",
            threadId: "thread-image",
            payload: {
              role: "user",
              text: "see attached",
              attachments: [
                { type: "image", src: "/api/assets/local-image/asset-1", name: "codex-clipboard.png" },
              ],
            },
          }),
        ]);
      }
      if (url === "http://bridge.local/api/assets/local-image/asset-1") {
        return new Response(new Blob(["png"], { type: "image/png" }), {
          status: 200,
          headers: { "Content-Type": "image/png" },
        });
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("see attached")).toBeInTheDocument();
    expect(await screen.findByRole("img", { name: "codex-clipboard.png" })).toHaveAttribute("src", "blob:codex-image");
    expect(globalThis.fetch).toHaveBeenCalledWith("http://bridge.local/api/assets/local-image/asset-1", {
      headers: { Authorization: "Bearer session-1" },
    });
  });

  it("shows_attachment_failure_when_image_proxy_rejects_asset", async () => {
    stubObjectUrls();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-image", title: "Image thread", preview: "Attachment" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-image/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-image",
            threadId: "thread-image",
            payload: {
              role: "user",
              text: "see attached",
              attachments: [
                { type: "image", src: "/api/assets/local-image/missing", name: "missing.png" },
              ],
            },
          }),
        ]);
      }
      if (url === "http://bridge.local/api/assets/local-image/missing") {
        return jsonResponse({ error: "asset not found" }, 404);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("see attached")).toBeInTheDocument();
    expect(await screen.findByText("Image unavailable: missing.png")).toBeInTheDocument();
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

  it("updates_same-id_assistant_message_with_new_text", () => {
    const initial = sessionEvent({
      id: "assistant-stream",
      threadId: "thread-a",
      payload: { role: "assistant", text: "Hel" },
    });
    const merged = appendOrMergeSessionEvent(
      [initial],
      sessionEvent({
        id: "assistant-stream",
        threadId: "thread-a",
        payload: { role: "assistant", text: "Hello" },
      }),
    );

    expect(merged).toHaveLength(1);
    expect(merged[0].payload).toEqual({ role: "assistant", text: "Hello" });
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

  it("polls_selected_thread_events_after_initial_load", async () => {
    saveActiveSession();
    let eventFetches = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-poll", title: "Polling target", preview: "Waiting" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-poll/events") {
        eventFetches += 1;
        return jsonResponse([
          sessionEvent({
            id: eventFetches === 1 ? "event-initial" : "event-polled",
            threadId: "thread-poll",
            payload: {
              role: "assistant",
              text: eventFetches === 1 ? "Initial load" : "Polled reply",
            },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("Initial load")).toBeInTheDocument();

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 2_100));
    });

    await waitFor(() => {
      expect(screen.getByText("Polled reply")).toBeInTheDocument();
    });
    expect(eventFetches).toBeGreaterThanOrEqual(2);
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
    expect(screen.queryByRole("heading", { name: "Pending approvals" })).not.toBeInTheDocument();

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

    expect(await screen.findByRole("heading", { name: "Pending approvals" })).toBeInTheDocument();
    expect(await screen.findByText("Run real check")).toBeInTheDocument();
  });

  it("replaces_non_pending_ws_events_when_http_snapshot_resolves_later", async () => {
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
    expect(screen.queryByText("Arrived over socket")).not.toBeInTheDocument();
  });

  it("keeps_event_stream_at_bottom_when_new_events_arrive_near_bottom", async () => {
    saveActiveSession();
    const scrollTo = vi.fn();
    HTMLElement.prototype.scrollTo = scrollTo;
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
        return jsonResponse([
          sessionEvent({
            id: "event-first",
            threadId: "thread-live",
            payload: { role: "assistant", text: "First" },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("First")).toBeInTheDocument();
    const stream = screen.getByLabelText("Session event stream");
    setScrollMetrics(stream, { scrollTop: 720, clientHeight: 200, scrollHeight: 900 });
    fireEvent.scroll(stream);
    act(() => {
      MockWebSocket.instances[0].emit({
        type: "session_event",
        payload: sessionEvent({
          id: "event-second",
          threadId: "thread-live",
          payload: { role: "assistant", text: "Second" },
          createdAt: 1_783_515_390_000,
        }),
      });
    });

    expect(await screen.findByText("Second")).toBeInTheDocument();
    expect(scrollTo).toHaveBeenLastCalledWith({ top: 900, behavior: "auto" });
  });

  it("keeps_bottom_when_polled_backfill_grows_event_list_without_changing_tail", async () => {
    saveActiveSession();
    const scrollTo = vi.fn();
    HTMLElement.prototype.scrollTo = scrollTo;
    let eventFetches = 0;
    let resolveBackfill: (response: Response) => void = () => {};
    const backfillResponse = new Promise<Response>((resolve) => {
      resolveBackfill = resolve;
    });
    const tailEvent = sessionEvent({
      id: "event-tail",
      threadId: "thread-live",
      payload: { role: "assistant", text: "Tail" },
      createdAt: 1_783_515_390_000,
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
        eventFetches += 1;
        if (eventFetches === 1) {
          return jsonResponse([tailEvent]);
        }
        return backfillResponse;
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("Tail")).toBeInTheDocument();
    const stream = screen.getByLabelText("Session event stream");
    setScrollMetrics(stream, { scrollTop: 720, clientHeight: 200, scrollHeight: 900 });
    fireEvent.scroll(stream);
    scrollTo.mockClear();

    await act(async () => {
      await new Promise((resolve) => window.setTimeout(resolve, 2_100));
    });
    setScrollMetrics(stream, { scrollTop: 720, clientHeight: 200, scrollHeight: 1_200 });
    await act(async () => {
      resolveBackfill(
        jsonResponse([
          sessionEvent({
            id: "event-backfill",
            threadId: "thread-live",
            payload: { role: "assistant", text: "Backfilled" },
            createdAt: 1_783_515_380_000,
          }),
          tailEvent,
        ]),
      );
    });

    expect(await screen.findByText("Backfilled")).toBeInTheDocument();
    expect(scrollTo).toHaveBeenLastCalledWith({ top: 1_200, behavior: "auto" });
  });

  it("does_not_steal_scroll_when_user_is_reading_history", async () => {
    saveActiveSession();
    const scrollTo = vi.fn();
    HTMLElement.prototype.scrollTo = scrollTo;
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
        return jsonResponse([
          sessionEvent({
            id: "event-first",
            threadId: "thread-live",
            payload: { role: "assistant", text: "First" },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("First")).toBeInTheDocument();
    const stream = screen.getByLabelText("Session event stream");
    scrollTo.mockClear();
    setScrollMetrics(stream, { scrollTop: 100, clientHeight: 200, scrollHeight: 900 });
    fireEvent.scroll(stream);
    act(() => {
      MockWebSocket.instances[0].emit({
        type: "session_event",
        payload: sessionEvent({
          id: "event-second",
          threadId: "thread-live",
          payload: { role: "assistant", text: "Second" },
          createdAt: 1_783_515_390_000,
        }),
      });
    });

    expect(await screen.findByText("Second")).toBeInTheDocument();
    expect(scrollTo).not.toHaveBeenCalled();
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

  it("keeps_polled_events_oldest_first_and_reconciles_pending_echo_with_newline", () => {
    const current = [
      sessionEvent({
        id: "old-assistant",
        threadId: "thread-a",
        payload: { role: "assistant", text: "Old answer" },
        createdAt: 1_783_515_380_000,
      }),
      sessionEvent({
        id: "local-pending",
        threadId: "thread-a",
        payload: { role: "user", text: "continue", pending: true },
        createdAt: 1_783_515_390_000,
      }),
    ];
    const polled = [
      sessionEvent({
        id: "turn-new:item-1",
        threadId: "thread-a",
        payload: { role: "user", text: "continue\n" },
        createdAt: 1_783_515_391_000,
      }),
      sessionEvent({
        id: "turn-new:item-2",
        threadId: "thread-a",
        payload: { role: "assistant", text: "New answer" },
        createdAt: 1_783_515_391_000,
      }),
      current[0],
    ];

    const merged = mergePolledSessionEvents(current, polled);

    expect(merged.map((event) => event.id)).toEqual([
      "old-assistant",
      "turn-new:item-1",
      "turn-new:item-2",
    ]);
    expect(merged.map((event) => event.payload).filter((payload) => payload && typeof payload === "object" && "text" in payload && payload.text === "continue")).toHaveLength(0);
  });

  it("treats_polled_events_as_authoritative_and_drops_stale_carried_ws_events", () => {
    const current = [
      sessionEvent({
        id: "turn-new:item-6",
        threadId: "thread-a",
        payload: { role: "assistant", text: "Canonical answer" },
        createdAt: 1_783_515_391_000,
      }),
      sessionEvent({
        id: "ws-progress-1",
        threadId: "thread-a",
        payload: { role: "assistant", text: "Local progress update" },
        createdAt: 1_783_515_392_000,
      }),
    ];
    const polled = [
      sessionEvent({
        id: "turn-new:item-1",
        threadId: "thread-a",
        payload: { role: "user", text: "question" },
        createdAt: 1_783_515_391_000,
      }),
      current[0],
    ];

    const merged = mergePolledSessionEvents(current, polled);

    expect(merged.map((event) => event.id)).toEqual(["turn-new:item-1", "turn-new:item-6"]);
  });

  it("keeps_unmatched_pending_user_message_after_polling", () => {
    const current = [
      sessionEvent({
        id: "local-pending",
        threadId: "thread-a",
        payload: { role: "user", text: "still sending", pending: true },
        createdAt: 1_783_515_392_000,
      }),
    ];
    const polled = [
      sessionEvent({
        id: "turn-old:item-2",
        threadId: "thread-a",
        payload: { role: "assistant", text: "Previous answer" },
        createdAt: 1_783_515_380_000,
      }),
    ];

    const merged = mergePolledSessionEvents(current, polled);

    expect(merged.map((event) => event.id)).toEqual(["turn-old:item-2", "local-pending"]);
  });

  it("orders_same_turn_events_by_item_number_when_timestamps_match", () => {
    const polled = [
      sessionEvent({
        id: "turn-new:item-6",
        threadId: "thread-a",
        payload: { role: "assistant", text: "Answer" },
        createdAt: 1_783_515_391_000,
      }),
      sessionEvent({
        id: "turn-new:item-1",
        threadId: "thread-a",
        payload: { role: "user", text: "Question" },
        createdAt: 1_783_515_391_000,
      }),
    ];

    const merged = mergePolledSessionEvents([], polled);

    expect(merged.map((event) => event.id)).toEqual(["turn-new:item-1", "turn-new:item-6"]);
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

function stubObjectUrls() {
  Object.defineProperty(URL, "createObjectURL", {
    configurable: true,
    value: vi.fn(() => "blob:codex-image"),
  });
  Object.defineProperty(URL, "revokeObjectURL", {
    configurable: true,
    value: vi.fn(),
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

function setScrollMetrics(
  element: Element,
  metrics: { scrollTop: number; clientHeight: number; scrollHeight: number },
) {
  Object.defineProperty(element, "scrollTop", {
    configurable: true,
    writable: true,
    value: metrics.scrollTop,
  });
  Object.defineProperty(element, "clientHeight", {
    configurable: true,
    value: metrics.clientHeight,
  });
  Object.defineProperty(element, "scrollHeight", {
    configurable: true,
    value: metrics.scrollHeight,
  });
}

function restoreScrollTo() {
  if (originalScrollTo) {
    HTMLElement.prototype.scrollTo = originalScrollTo;
    return;
  }
  delete (HTMLElement.prototype as Partial<HTMLElement>).scrollTo;
}

function restoreObjectUrls() {
  restoreUrlProperty("createObjectURL", originalCreateObjectURL);
  restoreUrlProperty("revokeObjectURL", originalRevokeObjectURL);
}

function restoreUrlProperty(
  name: "createObjectURL" | "revokeObjectURL",
  value: typeof URL.createObjectURL | typeof URL.revokeObjectURL | undefined,
) {
  if (value) {
    Object.defineProperty(URL, name, {
      configurable: true,
      value,
    });
    return;
  }
  delete (URL as unknown as Record<string, unknown>)[name];
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
