import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App, {
  appendOrMergeSessionEvent,
  connectionStateForError,
  mergeIncrementalSessionEvents,
  mergePolledSessionEvents,
  nextPollDelay,
} from "./App";
import type { SessionEvent, SessionSnapshot } from "@codex/bridge-protocol";
import { ApiError } from "./api";
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

  it("renders an empty unpaired workbench without demo data", () => {
    render(<App />);

    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Unpaired");
    expect(screen.getByRole("button", { name: "Open sessions" })).toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Pending approvals" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Sessions" })).toBeInTheDocument();
    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
    expect(screen.queryByText("Run npm install")).not.toBeInTheDocument();
    expect(screen.queryByText("Mobile bridge MVP")).not.toBeInTheDocument();
    expect(screen.queryByText("Bridge sidecar API")).not.toBeInTheDocument();
    expect(screen.getByText("No live sessions yet. Use the newest pairing URL from the bridge terminal.")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "No sessions available" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message No session selected")).toBeDisabled();
  });

  it("shows_the_running_bridge_version_from_health", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable", version: "9.9.9" });
      }
      if (url === "http://bridge.local/api/sessions" || url === "http://bridge.local/api/approvals") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("v9.9.9")).toBeInTheDocument();
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
    await user.click(within(drawer).getByRole("button", { name: /Live thread/ }));

    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Live thread" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Live thread")).toBeInTheDocument();
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

    expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.connection-bar\s*\{[\s\S]*grid-template-columns:\s*38px 38px minmax\(0, 1fr\) minmax\(0, auto\);/);
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

  it("does_not_duplicate_writable_connection_status", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([]);
      }
      return jsonResponse([]);
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");
    });
    expect(within(screen.getByLabelText("Connection status")).getAllByText("Writable")).toHaveLength(1);
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
    expect(globalThis.fetch).toHaveBeenCalledTimes(5);
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "http://stale.local/api/pairing/complete",
      expect.objectContaining({ method: "POST" }),
    );
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "http://bridge.local/api/session/refresh",
      expect.objectContaining({ method: "POST" }),
    );
    expect(globalThis.fetch).toHaveBeenCalledWith("http://bridge.local/api/approvals", {
      headers: { Authorization: "Bearer new-token" },
    });
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

  it("refreshes_session_when_session_list_rejects_token_after_bridge_restart", async () => {
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      const authorization = new Headers(init?.headers).get("Authorization");
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && authorization === "Bearer old-token") {
        return new Response(JSON.stringify({ message: "expired" }), { status: 401 });
      }
      if (url === "http://bridge.local/api/session/refresh" && init?.method === "POST") {
        return jsonResponse({
          deviceId: "device-1",
          sessionToken: "new-token",
          sessionExpiresAt: Date.now() + 120_000,
        });
      }
      if (url === "http://bridge.local/api/sessions" && authorization === "Bearer new-token") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-recovered",
            title: "Recovered thread",
            preview: "Session refreshed after restart",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-recovered/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByRole("heading", { name: "Recovered thread" });
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");
    expect(loadSession()?.sessionToken).toBe("new-token");
    expect(
      fetchSpy.mock.calls.some(
        ([input, init]) =>
          String(input) === "http://bridge.local/api/sessions" &&
          new Headers(init?.headers).get("Authorization") === "Bearer new-token",
      ),
    ).toBe(true);
  });

  it("refreshes_session_when_event_poll_rejects_token_after_bridge_restart", async () => {
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      const authorization = new Headers(init?.headers).get("Authorization");
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-recovered",
            title: "Recovered thread",
            preview: "Session refreshed after event auth failure",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-recovered/events" && authorization === "Bearer old-token") {
        return new Response(JSON.stringify({ message: "expired" }), { status: 401 });
      }
      if (url === "http://bridge.local/api/session/refresh" && init?.method === "POST") {
        return jsonResponse({
          deviceId: "device-1",
          sessionToken: "new-token",
          sessionExpiresAt: Date.now() + 120_000,
        });
      }
      if (url === "http://bridge.local/api/sessions/thread-recovered/events" && authorization === "Bearer new-token") {
        return jsonResponse([
          sessionEvent({
            id: "event-recovered",
            threadId: "thread-recovered",
            payload: { role: "assistant", text: "Recovered event stream" },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByText("Recovered event stream");
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");
    expect(loadSession()?.sessionToken).toBe("new-token");
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
    vi.setSystemTime(new Date("2026-07-09T13:00:00+08:00"));
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
            createdAt: new Date("2026-07-09T12:34:00+08:00").getTime(),
            payload: { role: "user", text: "Can you check this?" },
          }),
          sessionEvent({
            id: "event-assistant",
            threadId: "thread-roles",
            createdAt: new Date("2026-07-09T12:35:00+08:00").getTime(),
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
    expect(rows[0].querySelector("time")).toHaveTextContent("12:34");
    expect(rows[0].querySelector("time")).toHaveAttribute("dateTime", "2026-07-09T04:34:00.000Z");
    expect(rows[1]).toHaveClass("assistant");
    expect(rows[1]).toHaveTextContent("Codex");
    expect(rows[1].querySelector("time")).toHaveTextContent("12:35");
    expect(rows[1].querySelector("time")).toHaveAttribute("dateTime", "2026-07-09T04:35:00.000Z");
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

  it("defers_attachment_download_until_the_image_placeholder_is_visible", async () => {
    stubObjectUrls();
    saveActiveSession();
    let revealAttachment: (() => void) | null = null;
    vi.stubGlobal(
      "IntersectionObserver",
      class {
        constructor(callback: IntersectionObserverCallback) {
          revealAttachment = () => {
            callback(
              [{ isIntersecting: true } as IntersectionObserverEntry],
              this as unknown as IntersectionObserver,
            );
          };
        }

        observe() {}
        disconnect() {}
      },
    );
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
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
              text: "lazy attachment",
              attachments: [
                { type: "image", src: "/api/assets/local-image/asset-lazy", name: "lazy.png" },
              ],
            },
          }),
        ]);
      }
      if (url === "http://bridge.local/api/assets/local-image/asset-lazy") {
        return new Response(new Blob(["png"], { type: "image/png" }), {
          status: 200,
          headers: { "Content-Type": "image/png" },
        });
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("lazy attachment")).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalledWith(
      "http://bridge.local/api/assets/local-image/asset-lazy",
      expect.anything(),
    );

    act(() => {
      revealAttachment?.();
    });

    expect(await screen.findByRole("img", { name: "lazy.png" })).toHaveAttribute("src", "blob:codex-image");
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
          headers: expect.objectContaining({
            Authorization: "Bearer session-1",
            "Content-Type": "application/json",
            "X-Codex-Client-Message-Id": expect.any(String),
          }),
          body: JSON.stringify({ text: "continue from phone" }),
        }),
      );
    });
    expect(input).toHaveValue("");
  });

  it("removes_optimistic_message_and_keeps_draft_when_send_fails", async () => {
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
        return jsonResponse({ error: "temporary tunnel failure" }, 502);
      }
      return jsonResponse({});
    });

    render(<App />);

    const input = await screen.findByPlaceholderText("Message Reply target");
    await user.type(input, "retry from phone");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(await screen.findByRole("alert", undefined, { timeout: 3_000 })).toHaveTextContent(
      "Message not sent. Send message request failed with 502",
    );
    expect(input).toHaveValue("retry from phone");
    expect(
      within(screen.getByLabelText("Session event stream")).queryByText("retry from phone"),
    ).not.toBeInTheDocument();
  });

  it("send_message_can_include_image_attachment", async () => {
    const user = userEvent.setup();
    stubObjectUrls();
    saveActiveSession();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
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
    const fileInput = screen.getByLabelText("Choose image attachment");
    const image = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])], "phone.png", {
      type: "image/png",
    });
    await user.upload(fileInput, image);
    expect(screen.getByRole("img", { name: "phone.png" })).toHaveAttribute("src", "blob:codex-image");
    await user.type(input, "look at this");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        "http://bridge.local/api/sessions/thread-send/messages",
        expect.objectContaining({
          method: "POST",
          headers: expect.objectContaining({
            Authorization: "Bearer session-1",
            "Content-Type": "application/json",
            "X-Codex-Client-Message-Id": expect.any(String),
          }),
          body: JSON.stringify({
            text: "look at this",
            attachments: [
              {
                name: "phone.png",
                mimeType: "image/png",
                dataBase64: "iVBORw0KGgo=",
              },
            ],
          }),
        }),
      );
    });
    await waitFor(() => {
      expect(screen.queryByRole("img", { name: "phone.png" })).not.toBeInTheDocument();
    });
  });

  it("creates_new_session_from_the_phone_and_selects_it", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        return jsonResponse(
          sessionSnapshot({
            threadId: "thread-created",
            title: "Start from phone",
            preview: "Start from phone",
            status: "running",
          }),
          201,
        );
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/sessions/thread-created/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => {
      expect(newSessionButton).toBeEnabled();
    });
    await user.click(newSessionButton);
    await user.type(screen.getByLabelText("First message for new session"), "Start from phone");
    await user.click(screen.getByRole("button", { name: "Create & send" }));

    await waitFor(() => {
      expect(globalThis.fetch).toHaveBeenCalledWith(
        "http://bridge.local/api/sessions",
        expect.objectContaining({
          method: "POST",
          headers: {
            Authorization: "Bearer session-1",
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ text: "Start from phone" }),
        }),
      );
    });
    expect(screen.queryByRole("dialog", { name: "Start from phone" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Start from phone" })).toBeInTheDocument();
    expect(screen.getAllByText("Start from phone").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByPlaceholderText("Message Start from phone")).toBeInTheDocument();
  });

  it("keeps_new_session_failure_inside_sheet_without_replacing_current_thread", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        return jsonResponse({ error: "thread/start failed" }, 500);
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-existing", title: "Existing thread", preview: "Keep me selected" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-existing/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByRole("heading", { name: "Existing thread" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "New session" }));
    const textarea = screen.getByLabelText("First message for new session");
    await user.type(textarea, "This should stay in the sheet");
    await user.click(screen.getByRole("button", { name: "Create & send" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("Create session request failed with 500");
    expect(textarea).toHaveValue("This should stay in the sheet");
    expect(screen.getByRole("heading", { name: "Existing thread" })).toBeInTheDocument();
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");
  });

  it("polls_selected_thread_events_after_initial_load", async () => {
    saveActiveSession();
    let eventFetches = 0;
    const eventRequestHeaders: Headers[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
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
        eventRequestHeaders.push(new Headers(init?.headers));
        const initial = sessionEvent({
          id: "event-initial",
          threadId: "thread-poll",
          payload: { role: "assistant", text: "Initial load" },
        });
        return jsonResponse({
          events: eventFetches === 1
            ? [initial]
            : [
                initial,
                sessionEvent({
                  id: "event-polled",
                  threadId: "thread-poll",
                  payload: { role: "assistant", text: "Polled reply" },
                  createdAt: 1_783_515_390_000,
                }),
              ],
          beforeCursor: "event-initial",
          afterCursor: eventFetches === 1 ? "event-initial" : "event-polled",
          hasMoreBefore: false,
          hasMoreAfter: false,
          reset: false,
        });
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
    expect(screen.getByText("Initial load")).toBeInTheDocument();
    expect(eventFetches).toBeGreaterThanOrEqual(2);
    expect(eventRequestHeaders[0].get("X-Codex-Events-Limit")).toBe("50");
    expect(eventRequestHeaders[0].get("X-Codex-Events-Since")).toBeNull();
    expect(eventRequestHeaders[1].get("X-Codex-Events-Limit")).toBe("100");
    expect(eventRequestHeaders[1].get("X-Codex-Events-Since")).toBe("event-initial");
  });

  it("loads_earlier_messages_from_the_before_cursor", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    const eventRequestHeaders: Headers[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-history", title: "History target", preview: "Latest" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-history/events") {
        const headers = new Headers(init?.headers);
        eventRequestHeaders.push(headers);
        if (headers.get("X-Codex-Events-Before") === "event-3") {
          return jsonResponse({
            events: [
              sessionEvent({ id: "event-1", threadId: "thread-history", payload: { role: "user", text: "Old one" }, createdAt: 1 }),
              sessionEvent({ id: "event-2", threadId: "thread-history", payload: { role: "assistant", text: "Old two" }, createdAt: 2 }),
            ],
            beforeCursor: "event-1",
            afterCursor: "event-2",
            hasMoreBefore: false,
            hasMoreAfter: true,
            reset: false,
          });
        }
        return jsonResponse({
          events: [
            sessionEvent({ id: "event-3", threadId: "thread-history", payload: { role: "user", text: "Recent three" }, createdAt: 3 }),
            sessionEvent({ id: "event-4", threadId: "thread-history", payload: { role: "assistant", text: "Recent four" }, createdAt: 4 }),
          ],
          beforeCursor: "event-3",
          afterCursor: "event-4",
          hasMoreBefore: true,
          hasMoreAfter: false,
          reset: false,
        });
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByText("Recent four")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Load earlier messages" }));

    expect(await screen.findByText("Old one")).toBeInTheDocument();
    expect(screen.getByText("Old two")).toBeInTheDocument();
    expect(screen.getByText("Recent three")).toBeInTheDocument();
    expect(screen.getByText("Recent four")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load earlier messages" })).not.toBeInTheDocument();
    expect(eventRequestHeaders[1].get("X-Codex-Events-Limit")).toBe("50");
    expect(eventRequestHeaders[1].get("X-Codex-Events-Before")).toBe("event-3");
  });

  it("recovers_connection_status_after_visible_session_poll_succeeds", async () => {
    saveActiveSession();
    let sessionFetches = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        sessionFetches += 1;
        if (sessionFetches === 1) {
          return jsonResponse({ message: "phone slept" }, 503);
        }
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-recovered", title: "Recovered thread", preview: "Back online" }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-recovered/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Reconnecting");
    });
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Retrying automatically");
    expect(screen.getByLabelText("Connection status")).not.toHaveTextContent("Needs new link");

    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });

    expect(await screen.findByRole("heading", { name: "Recovered thread" })).toBeInTheDocument();
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");
  });

  it("reconnects_websocket_when_page_returns_to_foreground", async () => {
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
    expect(MockWebSocket.instances).toHaveLength(1);

    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });

    await waitFor(() => {
      expect(MockWebSocket.instances).toHaveLength(2);
    });
    expect(MockWebSocket.instances[0].closed).toBe(true);
  });

  it("calculates_adaptive_poll_delay_for_backoff_and_hidden_pages", () => {
    expect(nextPollDelay(2_000, 0, "visible")).toBe(2_000);
    expect(nextPollDelay(2_000, 2, "visible")).toBe(8_000);
    expect(nextPollDelay(2_000, 0, "hidden")).toBe(12_000);
    expect(nextPollDelay(5_000, 5, "hidden")).toBe(30_000);
  });

  it("only_suggests_a_new_link_after_three_transient_failures", () => {
    const error = new ApiError(502, "Sessions request failed with 502");

    expect(connectionStateForError(error, 1).label).toBe("Reconnecting");
    expect(connectionStateForError(error, 2).label).toBe("Reconnecting");
    expect(connectionStateForError(error, 3)).toEqual({
      label: "Connection error",
      detail:
        "Sessions request failed with 502. The public link has failed repeatedly; open the newest link from the Mac.",
    });
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

  it("renders_real_desktop_approval_from_authenticated_polling", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-approval",
            title: "Phone-created task",
            preview: "Waiting for MCP approval",
            status: "waiting_for_approval",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([
          {
            id: "thread-approval:7",
            threadId: "thread-approval",
            kind: "mcp",
            title: "Allow read_memory",
            detail: "uri: system://boot",
            riskHint: "MCP server: mcpServers",
            createdAt: 1_783_584_000_000,
          },
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-approval/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByRole("heading", { name: "Pending approvals" })).toBeInTheDocument();
    expect(await screen.findByText("Allow read_memory")).toBeInTheDocument();
    expect(screen.getByText("uri: system://boot")).toBeInTheDocument();
    expect(screen.getByText("MCP server: mcpServers")).toBeInTheDocument();
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

  it("reconciles_bridge_echo_that_arrives_before_new_session_optimistic_message", () => {
    const bridgeEcho = sessionEvent({
      id: "550e8400-e29b-41d4-a716-446655440000",
      threadId: "thread-new",
      payload: { role: "user", text: "same new task" },
      createdAt: 1_783_515_390_000,
    });
    const optimistic = sessionEvent({
      id: "local-new-session-1",
      threadId: "thread-new",
      payload: { role: "user", text: "same new task", pending: true },
      createdAt: 1_783_515_390_100,
    });

    const merged = appendOrMergeSessionEvent([bridgeEcho], optimistic);

    expect(merged).toHaveLength(1);
    expect(merged[0].id).toBe(bridgeEcho.id);
  });

  it("replaces_pending_and_uuid_bridge_echo_when_incremental_codex_turn_arrives", () => {
    const current = [
      sessionEvent({
        id: "local-1",
        threadId: "thread-send",
        payload: { role: "user", text: "今年世界杯什么时候结束", pending: true },
        createdAt: 1_783_515_390_000,
      }),
      sessionEvent({
        id: "550e8400-e29b-41d4-a716-446655440001",
        threadId: "thread-send",
        payload: { role: "user", text: "今年世界杯什么时候结束" },
        createdAt: 1_783_515_390_100,
      }),
    ];
    const incremental = [
      sessionEvent({
        id: "turn-new:item-1",
        threadId: "thread-send",
        payload: { role: "user", text: "今年世界杯什么时候结束" },
        createdAt: 1_783_515_389_000,
      }),
      sessionEvent({
        id: "turn-new:item-6",
        threadId: "thread-send",
        payload: { role: "assistant", text: "比赛将在 7 月结束。" },
        createdAt: 1_783_515_389_000,
      }),
    ];

    const merged = mergeIncrementalSessionEvents(current, incremental);

    expect(merged.map((event) => event.id)).toEqual(["turn-new:item-1", "turn-new:item-6"]);
    expect(
      merged.filter(
        (event) =>
          event.payload &&
          typeof event.payload === "object" &&
          !Array.isArray(event.payload) &&
          event.payload.role === "user" &&
          event.payload.text === "今年世界杯什么时候结束",
      ),
    ).toHaveLength(1);
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

  it("merges_incremental_event_pages_without_dropping_loaded_history", () => {
    const current = [
      sessionEvent({
        id: "event-1",
        payload: { role: "assistant", text: "Older history" },
        createdAt: 1_783_515_380_000,
      }),
      sessionEvent({
        id: "event-2",
        payload: { role: "assistant", text: "Working" },
        createdAt: 1_783_515_390_000,
      }),
    ];
    const incremental = [
      sessionEvent({
        id: "event-2",
        payload: { role: "assistant", text: "Finished" },
        createdAt: 1_783_515_390_000,
      }),
      sessionEvent({
        id: "event-3",
        payload: { role: "assistant", text: "New reply" },
        createdAt: 1_783_515_400_000,
      }),
    ];

    const merged = mergeIncrementalSessionEvents(current, incremental);

    expect(merged.map((event) => event.id)).toEqual(["event-1", "event-2", "event-3"]);
    expect(merged[1].payload).toMatchObject({ text: "Finished" });
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

  it("reconciles_image_only_pending_user_message_after_polling", () => {
    const current = [
      sessionEvent({
        id: "local-image-pending",
        threadId: "thread-a",
        payload: { role: "user", text: "", pending: true },
        createdAt: 1_783_515_392_000,
      }),
    ];
    const polled = [
      sessionEvent({
        id: "turn-image:item-1",
        threadId: "thread-a",
        payload: {
          role: "user",
          text: "",
          attachments: [{ type: "image", src: "/api/assets/local-image/asset-1", name: "phone.png" }],
        },
        createdAt: 1_783_515_393_000,
      }),
    ];

    const merged = mergePolledSessionEvents(current, polled);

    expect(merged.map((event) => event.id)).toEqual(["turn-image:item-1"]);
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
