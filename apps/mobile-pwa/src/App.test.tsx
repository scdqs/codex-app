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
import { groupSessionEventsForDisplay } from "./turn-groups";
import {
  clearProjectViewPreferences,
  clearSession,
  loadSession,
  saveSession,
} from "./storage";
import { dismissNotificationOnboarding } from "./notifications/onboarding-storage";

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
    clearProjectViewPreferences();
    localStorage.clear();
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
    expect(screen.getByRole("textbox", { name: "Message selected Codex session" })).toBeDisabled();
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

  it("uses_the_confirmed_two_level_mobile_header", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable", version: "9.9.9" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-header",
            title: "Header layout",
            status: "waiting_for_approval",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([
          {
            id: "approval-header",
            threadId: "thread-header",
            kind: "command",
            title: "Review header command",
            detail: "npm test",
            createdAt: 1_783_584_000_000,
          },
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-header/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const header = screen.getByLabelText("Connection status");
    expect(await within(header).findByRole("heading", { name: "Codex Mobile" })).toBeInTheDocument();
    const identity = header.querySelector(".connection-primary");
    expect(identity).not.toBeNull();
    expect(within(identity as HTMLElement).getByText("v9.9.9")).toBeInTheDocument();
    const rail = within(header).getByLabelText("Bridge status rail");
    expect(rail).toHaveTextContent("LAN bridge");
    expect(rail).not.toHaveTextContent("v9.9.9");
    expect(within(header).getAllByText("Writable")).toHaveLength(1);
    await waitFor(() => {
      expect(rail).toHaveTextContent("1 pending approval");
    });
  });

  it("leaves_the_message_composer_visually_empty_until_the_user_types", () => {
    render(<App />);

    const composer = screen.getByRole("form", { name: "Message composer" });
    const input = within(composer).getByRole("textbox", {
      name: "Message selected Codex session",
    });
    expect(input).not.toHaveAttribute("placeholder");
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
    await user.click(within(drawer).getByRole("button", { name: "Live thread" }));

    expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Live thread" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Message selected Codex session" })).toBeInTheDocument();
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
    const row = within(drawer).getByRole("button", { name: "Live thread" });
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

  it("opens_settings_from_the_drawer_and_preserves_the_selected_session_dom", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    dismissNotificationOnboarding("device-1");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/notification-settings") {
        return jsonResponse(notificationSettingsResponse());
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-live", title: "Live thread" }),
        ]);
      }
      if (url.endsWith("/events") || url.endsWith("/api/approvals")) {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);
    const sessionHeading = await screen.findByRole("heading", { name: "Live thread" });
    await user.click(screen.getByRole("button", { name: "Open sessions" }));
    await user.click(screen.getByRole("button", { name: "Open settings" }));

    expect(screen.getByRole("heading", { name: "Settings" })).toBeVisible();
    expect(sessionHeading).toBeInTheDocument();
    expect(sessionHeading).not.toBeVisible();

    await user.click(screen.getByRole("button", { name: "Back to workbench" }));
    expect(sessionHeading).toBeVisible();
  });

  it("deduplicates_foreground_alert_events_from_websocket", async () => {
    saveActiveSession();
    dismissNotificationOnboarding("device-1");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/notification-settings") {
        return jsonResponse(
          notificationSettingsResponse({ enabled: true, soundEnabled: true }),
        );
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([sessionSnapshot({ threadId: "thread-live", title: "Live thread" })]);
      }
      if (url.endsWith("/events") || url.endsWith("/api/approvals")) {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);
    await screen.findByRole("heading", { name: "Live thread" });
    await waitFor(() => expect(MockWebSocket.instances).toHaveLength(1));
    const alertEnvelope = {
      type: "alert_event",
      payload: {
        eventId: "alert-once",
        kind: "completed",
        threadId: "thread-live",
        threadTitle: "Live thread",
        occurredAt: Date.now(),
      },
    };

    act(() => MockWebSocket.instances[0].emit(alertEnvelope));
    expect(await screen.findByRole("button", { name: "Tap to enable sound" })).toBeInTheDocument();
    act(() => MockWebSocket.instances[0].emit(alertEnvelope));
    expect(screen.getAllByRole("button", { name: "Tap to enable sound" })).toHaveLength(1);
  });

  it("deduplicates_the_same_alert_between_websocket_and_service_worker", async () => {
    saveActiveSession();
    dismissNotificationOnboarding("device-1");
    const serviceWorker = new MockServiceWorkerContainer();
    const vibrate = vi.fn();
    stubNavigator({ serviceWorker, vibrate });
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/notification-settings") {
        return jsonResponse(
          notificationSettingsResponse({
            enabled: true,
            soundEnabled: false,
            foregroundVibration: true,
          }),
        );
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([sessionSnapshot({ threadId: "thread-live", title: "Live thread" })]);
      }
      if (url.endsWith("/events") || url.endsWith("/api/approvals")) {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);
    await screen.findByRole("heading", { name: "Live thread" });
    await waitFor(() => expect(MockWebSocket.instances).toHaveLength(1));
    const alert = {
      eventId: "alert-cross-channel",
      kind: "completed",
      threadId: "thread-live",
      threadTitle: "Live thread",
      occurredAt: Date.now(),
    };

    act(() => MockWebSocket.instances[0].emit({ type: "alert_event", payload: alert }));
    act(() => serviceWorker.emit({ type: "codex_alert_event", payload: alert }));

    await waitFor(() => expect(vibrate).toHaveBeenCalledTimes(1));
  });

  it("opens_the_thread_from_a_notification_query_after_sessions_load", async () => {
    saveActiveSession();
    dismissNotificationOnboarding("device-1");
    window.history.replaceState(null, "", "/?threadId=thread-target");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/notification-settings") {
        return jsonResponse(notificationSettingsResponse());
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({ threadId: "thread-other", title: "Other thread" }),
          sessionSnapshot({ threadId: "thread-target", title: "Target thread" }),
        ]);
      }
      if (url.endsWith("/events") || url.endsWith("/api/approvals")) {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByRole("heading", { name: "Target thread" })).toBeInTheDocument();
    expect(new URL(window.location.href).searchParams.has("threadId")).toBe(false);
  });

  it("keeps_the_current_thread_and_reports_a_stale_notification_link", async () => {
    saveActiveSession();
    dismissNotificationOnboarding("device-1");
    window.history.replaceState(null, "", "/?threadId=thread-missing");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/notification-settings") {
        return jsonResponse(notificationSettingsResponse());
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([sessionSnapshot({ threadId: "thread-live", title: "Live thread" })]);
      }
      if (url.endsWith("/events") || url.endsWith("/api/approvals")) {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByRole("heading", { name: "Live thread" })).toBeInTheDocument();
    expect(await screen.findByRole("alert")).toHaveTextContent("Session is no longer available");
    expect(new URL(window.location.href).searchParams.has("threadId")).toBe(false);
  });

  it("enables_and_disables_system_notifications_through_the_complete_browser_flow", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    dismissNotificationOnboarding("device-1");
    const push = stubFixedPushEnvironment();
    const requestOrder: string[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/notification-settings" && method === "GET") {
        return jsonResponse(
          notificationSettingsResponse({
            fixedHttps: true,
            deliveryMode: "web_push",
            systemNotifications: true,
            subscriptionState: "not_enabled",
          }),
        );
      }
      if (url === "http://bridge.local/api/push/public-key") {
        return jsonResponse({ publicKey: "BBDh6d4q3G2c9vl9IK2JvlfqubT4Lpi0JYYA-mock-public-key" });
      }
      if (url === "http://bridge.local/api/push/subscription" && method === "POST") {
        requestOrder.push("save-subscription");
        return new Response(null, { status: 201 });
      }
      if (url === "http://bridge.local/api/notification-settings" && method === "PUT") {
        const enabled = JSON.parse(String(init?.body)).enabled as boolean;
        requestOrder.push(enabled ? "enable-master" : "disable-master");
        return jsonResponse(
          notificationSettingsResponse({
            enabled,
            fixedHttps: true,
            deliveryMode: "web_push",
            systemNotifications: true,
            subscriptionState: enabled ? "active" : "active",
          }),
        );
      }
      if (url === "http://bridge.local/api/push/subscription" && method === "DELETE") {
        requestOrder.push("delete-subscription");
        return new Response(null, { status: 204 });
      }
      if (url === "http://bridge.local/api/notifications/test") {
        requestOrder.push("test-alert");
        return jsonResponse({
          eventId: "test-alert-1",
          kind: "completed",
          threadId: "thread-live",
          threadTitle: "Live thread",
          occurredAt: Date.now(),
        });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([sessionSnapshot({ threadId: "thread-live", title: "Live thread" })]);
      }
      if (url.endsWith("/events") || url.endsWith("/api/approvals")) {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);
    await screen.findByRole("heading", { name: "Live thread" });
    await user.click(screen.getByRole("button", { name: "Open sessions" }));
    await user.click(screen.getByRole("button", { name: "Open settings" }));
    await user.click(await screen.findByRole("button", { name: "Enable system notifications" }));

    expect(await screen.findByText("Active")).toBeInTheDocument();
    expect(push.requestPermission).toHaveBeenCalledTimes(1);
    expect(push.subscribe).toHaveBeenCalledTimes(1);
    expect(requestOrder.slice(0, 3)).toEqual([
      "save-subscription",
      "enable-master",
      "test-alert",
    ]);

    await user.click(screen.getByRole("button", { name: "Disable alerts" }));

    expect(await screen.findByText("Not enabled")).toBeInTheDocument();
    expect(requestOrder.indexOf("disable-master")).toBeLessThan(
      requestOrder.indexOf("delete-subscription"),
    );
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

    expect(css).toMatch(/\.connection-main-row\s*\{[^}]*grid-template-columns:\s*auto minmax\(0, 1fr\) auto;/);
    expect(css).toMatch(/\.connection-status-rail\s*\{[^}]*justify-content:\s*space-between;/);
    expect(css).toMatch(/\.session-menu-button\s*\{[^}]*display:\s*grid;/);
    expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.desktop-session-panel\s*\{[\s\S]*display:\s*none;/);
    expect(css).toMatch(/\.session-drawer\s*\{[^}]*width:\s*min\(84vw, 340px\);/);
    expect(css).not.toContain("grid-template-rows: minmax(150px, 0.42fr) minmax(0, 1fr)");
  });

  it("reserves_more_mobile_height_for_the_conversation_and_bounds_approval_content", () => {
    const stylesUrl = new URL("./styles.css", import.meta.url);
    const stylesPath =
      stylesUrl.protocol === "file:"
        ? stylesUrl
        : stylesUrl.pathname.startsWith("/@fs/")
          ? stylesUrl.pathname.slice("/@fs".length)
          : `.${stylesUrl.pathname}`;
    const css = readFileSync(stylesPath, "utf8");

    expect(css).toContain("--composer-height: 66px");
    expect(css).toMatch(/\.composer\s*\{[^}]*padding:\s*6px 8px calc\(6px \+ var\(--safe-bottom\)\);/);
    expect(css).toMatch(/\.approval-detail\.expanded\s*\{[^}]*max-height:\s*min\(34dvh, 280px\);[^}]*overflow-y:\s*auto;/);
    expect(css).toMatch(/\.approval-actions\s*\{[^}]*position:\s*relative;/);
  });

  it("keeps_the_sessions_drawer_and_settings_entry_reachable_on_wide_screens", () => {
    const stylesUrl = new URL("./styles.css", import.meta.url);
    const stylesPath =
      stylesUrl.protocol === "file:"
        ? stylesUrl
        : stylesUrl.pathname.startsWith("/@fs/")
          ? stylesUrl.pathname.slice("/@fs".length)
          : `.${stylesUrl.pathname}`;
    const css = readFileSync(stylesPath, "utf8");
    const baseCss = css.slice(0, css.indexOf("@media (max-width: 720px)"));

    expect(baseCss).toMatch(/\.session-menu-button\s*\{[^}]*display:\s*grid;/);
    expect(baseCss).toMatch(/\.session-drawer-layer\s*\{[^}]*position:\s*fixed;[^}]*display:\s*block;/);
    expect(baseCss).toMatch(/\.session-drawer\s*\{[^}]*width:\s*min\(84vw, 340px\);/);
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

  it("opens_the_full_connection_message_in_a_bottom_sheet", async () => {
    const user = userEvent.setup();
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

    const trigger = await screen.findByRole("button", { name: "Show connection details" });
    expect(trigger).toHaveAttribute("aria-expanded", "false");
    await user.click(trigger);

    const dialog = screen.getByRole("dialog", { name: "Connection details" });
    expect(trigger).toHaveAttribute("aria-expanded", "true");
    expect(dialog).toHaveTextContent("Session revoked or expired · Needs new link");
    expect(within(dialog).getByText("LAN bridge")).toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "Close connection details" }));

    expect(screen.queryByRole("dialog", { name: "Connection details" })).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("hides_sample_data_when_pairing_link_is_invalid", async () => {
    window.history.replaceState(
      null,
      "",
      "/?pairingToken=used-token&bridgeUrl=http%3A%2F%2Fbridge.local",
    );
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      jsonResponse({ code: "invalid_pairing_token", error: "invalid pairing token" }, 400),
    );

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
    expect(globalThis.fetch).toHaveBeenCalledTimes(6);
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
      expect(screen.getByRole("button", { name: "Implement bridge UI" })).toBeInTheDocument();
    });
    expect(await screen.findByText("Loaded first thread.")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Review sidecar API" }));

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Review sidecar API" })).toBeInTheDocument();
    });
    expect(screen.getByText("Loaded second thread.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Review sidecar API" })).toHaveAttribute("aria-current", "true");
  });

  it("prefers_the_recent_root_session_over_a_newer_subagent_on_initial_load", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          {
            ...sessionSnapshot({
              threadId: "thread-subagent",
              title: "Task 6 Mode Implementer · Raman the 2nd",
              preview: "Internal worker",
              updatedAt: 300,
              status: "running",
            }),
            isSubagent: true,
          },
          {
            ...sessionSnapshot({
              threadId: "thread-root",
              title: "同步 Codex 回复过程到手机",
              preview: "Main conversation",
              updatedAt: 200,
              status: "running",
            }),
            isSubagent: false,
          },
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-root/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-root",
            threadId: "thread-root",
            payload: { role: "assistant", text: "Loaded the main conversation." },
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-subagent/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-subagent",
            threadId: "thread-subagent",
            payload: { role: "assistant", text: "Loaded the internal worker." },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(
      await screen.findByRole("heading", { name: "同步 Codex 回复过程到手机" }),
    ).toBeInTheDocument();
    expect(await screen.findByText("Loaded the main conversation.")).toBeInTheDocument();
    expect(screen.queryByText("Loaded the internal worker.")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Task 6 Mode Implementer · Raman the 2nd" }),
    ).not.toBeInTheDocument();

    await waitFor(() => expect(MockWebSocket.instances).toHaveLength(1));
    act(() => {
      MockWebSocket.instances[0].emit({
        type: "session_snapshot",
        payload: sessionSnapshot({
          threadId: "thread-subagent",
          title: "Task 6 Mode Implementer · Raman the 2nd",
          preview: "Internal worker updated over WebSocket",
          updatedAt: 400,
          status: "running",
        }),
      });
    });

    expect(
      screen.queryByRole("button", { name: "Task 6 Mode Implementer · Raman the 2nd" }),
    ).not.toBeInTheDocument();
  });

  it("prefers_a_rich_root_session_over_a_newer_unresolved_uuid_snapshot", async () => {
    saveActiveSession();
    const unresolvedThreadId = "019f78a4-f383-7813-96e0-522b5feb06c7";
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: unresolvedThreadId,
            title: unresolvedThreadId,
            cwd: undefined,
            modelProvider: undefined,
            preview: undefined,
            updatedAt: 300,
            status: "running",
          }),
          sessionSnapshot({
            threadId: "thread-root",
            title: "修复首次配对会话",
            cwd: "/repo",
            modelProvider: "openai",
            preview: "Main conversation",
            updatedAt: 200,
            status: "running",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-root/events") {
        return jsonResponse([
          sessionEvent({
            id: "event-root",
            threadId: "thread-root",
            payload: { role: "assistant", text: "Loaded the resolved main conversation." },
          }),
        ]);
      }
      if (url === `http://bridge.local/api/sessions/${unresolvedThreadId}/events`) {
        return jsonResponse([
          sessionEvent({
            id: "event-unresolved",
            threadId: unresolvedThreadId,
            payload: { role: "assistant", text: "Loaded the unresolved thread." },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByRole("heading", { name: "修复首次配对会话" })).toBeInTheDocument();
    expect(await screen.findByText("Loaded the resolved main conversation.")).toBeInTheDocument();
    expect(screen.queryByText("Loaded the unresolved thread.")).not.toBeInTheDocument();
  });

  it("groups_sessions_by_project_and_restores_local_view_preferences", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-new",
            title: "Newer session",
            cwd: "/Users/damon/repo/app",
            updatedAt: 200,
          }),
          sessionSnapshot({
            threadId: "thread-old",
            title: "Pinned session",
            cwd: "/Users/damon/repo/app/",
            updatedAt: 100,
          }),
        ]);
      }
      if (url.endsWith("/events")) {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    const firstRender = render(<App />);

    const project = await screen.findByRole("region", { name: "app project" });
    const projectToggle = within(project).getByRole("button", { expanded: true });
    expect(projectToggle).toHaveAttribute("title", "/Users/damon/repo/app");
    expect(projectToggle).toHaveTextContent("2");

    await user.click(within(project).getByRole("button", { name: "Pin Pinned session" }));
    expect(sessionButtonNames(project)).toEqual(["Pinned session", "Newer session"]);

    await user.click(projectToggle);
    expect(within(project).queryByRole("button", { name: "Pinned session" })).not.toBeInTheDocument();

    firstRender.unmount();
    render(<App />);

    const restoredProject = await screen.findByRole("region", { name: "app project" });
    expect(within(restoredProject).getByRole("button", { expanded: false })).toBeInTheDocument();

    await user.click(within(restoredProject).getByRole("button", { expanded: false }));
    expect(sessionButtonNames(restoredProject)).toEqual(["Pinned session", "Newer session"]);
    expect(within(restoredProject).getByRole("button", { name: "Unpin Pinned session" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
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

  it("keeps_answer_reasoning_summary_and_plan_streams_separate", () => {
    const streamed = [
      sessionEvent({
        id: "turn-1:reasoning-1",
        type: "reasoning_summary_delta",
        payload: { text: "Checking " },
      }),
      sessionEvent({
        id: "turn-1:reasoning-1",
        type: "reasoning_summary_delta",
        payload: { text: "tests" },
      }),
      sessionEvent({
        id: "turn-1:plan-1",
        type: "plan_delta",
        payload: { text: "Run regression" },
      }),
      sessionEvent({
        id: "turn-1:message-1",
        type: "message_delta",
        payload: { text: "Done" },
      }),
    ].reduce<SessionEvent[]>((events, event) => appendOrMergeSessionEvent(events, event), []);

    expect(streamed.map((event) => event.type)).toEqual([
      "reasoning_summary",
      "plan",
      "message",
    ]);
    expect(streamed.map((event) => event.payload)).toEqual([
      { role: "reasoning", text: "Checking tests" },
      { role: "plan", text: "Run regression" },
      { role: "assistant", text: "Done" },
    ]);
  });

  it("renders_running_reasoning_summary_expanded_and_user_collapsible", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-stream",
            title: "Streaming task",
            status: "running",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-stream/events") {
        return jsonResponse([
          sessionEvent({
            id: "turn-1:reasoning-1",
            threadId: "thread-stream",
            type: "reasoning_summary",
            payload: { role: "reasoning", text: "Reviewing the implementation" },
          }),
          sessionEvent({
            id: "turn-1:plan-1",
            threadId: "thread-stream",
            type: "plan",
            payload: { role: "plan", text: "Run the focused tests" },
          }),
          sessionEvent({
            id: "turn-1:tool-result-empty",
            threadId: "thread-stream",
            type: "tool_result",
            payload: { role: "tool_result", text: "" },
          }),
          sessionEvent({
            id: "turn-1:message-1",
            threadId: "thread-stream",
            payload: { role: "assistant", text: "The change is ready." },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const thinking = await screen.findByText("Thinking");
    const details = thinking.closest("details");
    expect(details).toHaveAttribute("open");
    expect(screen.getByText("Reviewing the implementation")).toBeInTheDocument();
    expect(screen.getByText("Run the focused tests")).toBeInTheDocument();
    expect(screen.getByText("The change is ready.")).toBeInTheDocument();
    const responses = screen.getAllByRole("article", { name: "Codex response" });
    expect(responses).toHaveLength(1);
    expect(within(responses[0]).getByText("Reviewing the implementation")).toBeInTheDocument();
    expect(within(responses[0]).getByText("Run the focused tests")).toBeInTheDocument();
    expect(within(responses[0]).getByText("The change is ready.")).toBeInTheDocument();
    expect(within(responses[0]).queryByText("tool result", { exact: false })).not.toBeInTheDocument();

    await user.click(thinking);
    expect(details).not.toHaveAttribute("open");
  });

  it("renders_semantic_tool_progress_inside_the_codex_turn", async () => {
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-tools",
            title: "Tool progress",
            status: "running",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-tools/events") {
        return jsonResponse([
          sessionEvent({
            id: "turn-1:search-1",
            threadId: "thread-tools",
            type: "tool_call",
            payload: {
              role: "tool",
              text: "Searching files: codex-manual.md in my_ai",
              title: "Searching files",
              detail: "codex-manual.md in my_ai",
              toolKind: "search",
              toolStatus: "running",
              turnId: "turn-1",
            },
          }),
          sessionEvent({
            id: "turn-1:edit-1",
            threadId: "thread-tools",
            type: "tool_result",
            payload: {
              role: "tool_result",
              text: "Updated files: App.tsx, styles.css",
              title: "Updated files",
              detail: "App.tsx, styles.css",
              toolKind: "file_change",
              toolStatus: "completed",
              turnId: "turn-1",
            },
          }),
          sessionEvent({
            id: "turn-1:build-1",
            threadId: "thread-tools",
            type: "tool_result",
            payload: {
              role: "tool_result",
              text: "Build failed",
              title: "Build failed",
              toolKind: "build",
              toolStatus: "failed",
              turnId: "turn-1",
            },
          }),
          sessionEvent({
            id: "turn-1:edit-2",
            threadId: "thread-tools",
            type: "tool_result",
            payload: {
              role: "tool_result",
              text: "Skipped file update",
              title: "Skipped file update",
              toolKind: "file_change",
              toolStatus: "declined",
              turnId: "turn-1",
            },
          }),
        ]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const response = await screen.findByRole("article", { name: "Codex response" });
    expect(within(response).getByText("Searching files")).toBeInTheDocument();
    expect(within(response).getByText("codex-manual.md in my_ai")).toBeInTheDocument();
    expect(within(response).getByText("Updated files")).toBeInTheDocument();
    expect(within(response).getByText("App.tsx, styles.css")).toBeInTheDocument();
    expect(within(response).getByText("Build failed").closest(".tool-activity")).toHaveClass("failed");
    expect(within(response).getByText("Skipped file update").closest(".tool-activity")).toHaveClass("declined");
    expect(within(response).getAllByLabelText("Tool activity")).toHaveLength(4);
    expect(within(response).queryByText("tool call", { exact: false })).not.toBeInTheDocument();
    expect(within(response).queryByText("tool result", { exact: false })).not.toBeInTheDocument();
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

  it("replaces_running_tool_activity_with_its_completed_state", () => {
    const running = sessionEvent({
      id: "turn-1:item-3",
      threadId: "thread-tools",
      type: "tool_call",
      payload: {
        role: "tool",
        text: "Searching files: codex-manual.md in my_ai",
        title: "Searching files",
        detail: "codex-manual.md in my_ai",
        toolKind: "search",
        toolStatus: "running",
        turnId: "turn-1",
      },
    });
    const completed = sessionEvent({
      id: "turn-1:item-3",
      threadId: "thread-tools",
      type: "tool_result",
      payload: {
        role: "tool_result",
        text: "Searched files: codex-manual.md in my_ai",
        title: "Searched files",
        detail: "codex-manual.md in my_ai",
        toolKind: "search",
        toolStatus: "completed",
        turnId: "turn-1",
      },
    });

    const merged = appendOrMergeSessionEvent([running], completed);

    expect(merged).toHaveLength(1);
    expect(merged[0]).toEqual(completed);
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

    const input = await screen.findByRole("textbox", { name: "Message selected Codex session" });
    await waitFor(() => expect(input).toBeEnabled());
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

    const input = await screen.findByRole("textbox", { name: "Message selected Codex session" });
    await waitFor(() => expect(input).toBeEnabled());
    await user.type(input, "retry from phone");
    await user.click(screen.getByRole("button", { name: "Send message" }));

    expect(await screen.findByRole("alert", undefined, { timeout: 3_000 })).toHaveTextContent(
      "Message not sent. temporary tunnel failure",
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

    const input = await screen.findByRole("textbox", { name: "Message selected Codex session" });
    await waitFor(() => expect(input).toBeEnabled());
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
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/mobile" }]);
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
          body: JSON.stringify({ text: "Start from phone", cwd: "/repo/mobile" }),
        }),
      );
    });
    expect(screen.queryByRole("dialog", { name: "Start from phone" })).not.toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Start from phone" })).toBeInTheDocument();
    expect(screen.getAllByText("Start from phone").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByRole("textbox", { name: "Message selected Codex session" })).toBeInTheDocument();
  });

  it("creates_a_new_session_with_an_image_attachment", async () => {
    const user = userEvent.setup();
    stubObjectUrls();
    saveActiveSession();
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        return jsonResponse(
          sessionSnapshot({
            threadId: "thread-image-created",
            title: "Image task",
            preview: "Image task",
            status: "running",
            cwd: "/repo/mobile",
          }),
          201,
        );
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/mobile" }]);
      }
      if (url === "http://bridge.local/api/sessions/thread-image-created/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);
    const image = new File(
      [new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])],
      "phone.png",
      { type: "image/png" },
    );
    await user.upload(screen.getByLabelText("Choose new session image attachment"), image);
    expect(screen.getByRole("img", { name: "phone.png" })).toHaveAttribute("src", "blob:codex-image");
    expect(screen.getByRole("button", { name: "Create & send" })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: "Create & send" }));

    await waitFor(() => {
      expect(fetchMock).toHaveBeenCalledWith(
        "http://bridge.local/api/sessions",
        expect.objectContaining({
          method: "POST",
          body: JSON.stringify({
            text: "",
            attachments: [
              {
                name: "phone.png",
                mimeType: "image/png",
                dataBase64: "iVBORw0KGgo=",
              },
            ],
            cwd: "/repo/mobile",
          }),
        }),
      );
    });
    expect(await screen.findByRole("heading", { name: "Image task" })).toBeInTheDocument();
    expect(screen.queryByRole("img", { name: "phone.png" })).not.toBeInTheDocument();
  });

  it("defaults_new_session_workspace_to_the_current_session_cwd", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-current",
            title: "Current thread",
            cwd: "/repo/current",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/other" }, { cwd: "/repo/current" }]);
      }
      if (url === "http://bridge.local/api/sessions/thread-current/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByRole("heading", { name: "Current thread" });
    await user.click(screen.getByRole("button", { name: "New session" }));

    expect(await screen.findByLabelText("Workspace")).toHaveValue("/repo/current");
  });

  it("waits_for_session_data_before_enabling_new_session_creation", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    let resolveSessions: ((response: Response) => void) | undefined;
    const sessionsResponse = new Promise<Response>((resolve) => {
      resolveSessions = resolve;
    });
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return sessionsResponse;
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/other" }, { cwd: "/repo/current" }]);
      }
      if (url === "http://bridge.local/api/sessions/thread-current/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = screen.getByRole("button", { name: "New session" });
    expect(newSessionButton).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Open sessions" }));
    expect(within(screen.getByRole("dialog", { name: "Sessions" })).getByRole("button", { name: "New" })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "Close sessions" }));

    await act(async () => {
      resolveSessions?.(
        jsonResponse([
          sessionSnapshot({
            threadId: "thread-current",
            title: "Current thread",
            cwd: "/repo/current",
          }),
        ]),
      );
      await sessionsResponse;
    });

    await screen.findByRole("heading", { name: "Current thread" });
    expect(newSessionButton).toBeEnabled();
    await user.click(newSessionButton);
    expect(await screen.findByLabelText("Workspace")).toHaveValue("/repo/current");
  });

  it("requires_an_explicit_workspace_when_multiple_options_have_no_current_context", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/alpha" }, { cwd: "/repo/beta" }]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);
    await user.type(screen.getByLabelText("First message for new session"), "Choose carefully");
    const workspace = await screen.findByLabelText("Workspace");

    expect(workspace).toHaveValue("");
    expect(screen.getByRole("button", { name: "Create & send" })).toBeDisabled();
    await user.selectOptions(workspace, "/repo/beta");
    expect(screen.getByRole("button", { name: "Create & send" })).toBeEnabled();
  });

  it("shows_an_empty_workspace_state_and_disables_new_session_creation", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" || url === "http://bridge.local/api/workspaces") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);

    expect(await screen.findByText("No safe workspaces are available from existing Codex sessions.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create & send" })).toBeDisabled();
  });

  it("retries_workspace_loading_without_clearing_the_new_session_draft", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    let workspaceRequests = 0;
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        workspaceRequests += 1;
        return workspaceRequests === 1
          ? jsonResponse({ code: "adapter_error", error: "thread list failed" }, 502)
          : jsonResponse([{ cwd: "/repo/recovered" }]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);
    const textarea = screen.getByLabelText("First message for new session");
    await user.type(textarea, "Keep this draft");

    expect(await screen.findByRole("alert")).toHaveTextContent("thread list failed");
    await user.click(screen.getByRole("button", { name: "Retry workspaces" }));

    expect(await screen.findByLabelText("Workspace")).toHaveValue("/repo/recovered");
    expect(textarea).toHaveValue("Keep this draft");
  });

  it("refreshes_session_once_when_workspace_list_rejects_the_saved_token", async () => {
    const user = userEvent.setup();
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      const authorization = new Headers(init?.headers).get("Authorization");
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" || url === "http://bridge.local/api/approvals") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces" && authorization === "Bearer old-token") {
        return jsonResponse({ code: "unauthorized", error: "expired" }, 401);
      }
      if (url === "http://bridge.local/api/session/refresh" && init?.method === "POST") {
        return jsonResponse({
          deviceId: "device-1",
          sessionToken: "new-token",
          sessionExpiresAt: Date.now() + 120_000,
        });
      }
      if (url === "http://bridge.local/api/workspaces" && authorization === "Bearer new-token") {
        return jsonResponse([{ cwd: "/repo/refreshed" }]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);

    expect(await screen.findByLabelText("Workspace")).toHaveValue("/repo/refreshed");
    expect(loadSession()?.sessionToken).toBe("new-token");
    expect(
      fetchMock.mock.calls.filter(
        ([input, init]) =>
          String(input) === "http://bridge.local/api/session/refresh" && init?.method === "POST",
      ),
    ).toHaveLength(1);
  });

  it("leaves_pairing_state_when_session_refresh_fails", async () => {
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    let refreshRequests = 0;
    let rejectFirstRefresh: ((response: Response) => void) | undefined;
    const firstRefresh = new Promise<Response>((resolve) => {
      rejectFirstRefresh = resolve;
    });
    const stalledRefresh = new Promise<Response>(() => {});
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-revoked",
            title: "Revoked device thread",
            preview: "Should be cleared after revocation",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([
          {
            id: "approval-revoked",
            threadId: "thread-revoked",
            kind: "command",
            title: "Stale approval",
            detail: "Should be cleared after revocation",
            createdAt: 1_784_270_000_000,
          },
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-revoked/events") {
        return jsonResponse({ code: "unauthorized", error: "expired" }, 401);
      }
      if (url === "http://bridge.local/api/session/refresh" && init?.method === "POST") {
        refreshRequests += 1;
        return refreshRequests === 1 ? firstRefresh : stalledRefresh;
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(await screen.findByRole("heading", { name: "Revoked device thread" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Pending approvals" })).toBeInTheDocument();
    await waitFor(() => {
      expect(refreshRequests).toBe(1);
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Refreshing session");
    });
    await act(async () => {
      rejectFirstRefresh?.(jsonResponse({ code: "adapter_error", error: "refresh unavailable" }, 502));
    });
    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Reconnecting");
    });
    await new Promise((resolve) => window.setTimeout(resolve, 50));

    expect(refreshRequests).toBe(1);
    expect(screen.getByLabelText("Connection status")).not.toHaveTextContent("Refreshing session");
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("refresh unavailable");
  });

  it("stops_polling_when_device_session_refresh_is_rejected", async () => {
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    let refreshRequests = 0;
    let rejectFirstRefresh: ((response: Response) => void) | undefined;
    const firstRefresh = new Promise<Response>((resolve) => {
      rejectFirstRefresh = resolve;
    });
    const stalledRefresh = new Promise<Response>(() => {});
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse({ code: "unauthorized", error: "expired" }, 401);
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/session/refresh" && init?.method === "POST") {
        refreshRequests += 1;
        return refreshRequests === 1
          ? firstRefresh
          : stalledRefresh;
      }
      return jsonResponse({});
    });

    render(<App />);

    await waitFor(() => {
      expect(refreshRequests).toBe(1);
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Refreshing session");
    });
    await act(async () => {
      rejectFirstRefresh?.(jsonResponse({ code: "unauthorized", error: "unauthorized" }, 401));
    });
    await waitFor(() => {
      expect(screen.getByLabelText("Connection status")).toHaveTextContent("Session revoked or expired");
    });
    await new Promise((resolve) => window.setTimeout(resolve, 50));

    expect(refreshRequests).toBe(1);
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Needs new link");
    expect(screen.getByLabelText("Connection status")).not.toHaveTextContent("Refreshing session");
    expect(screen.queryByRole("heading", { name: "Revoked device thread" })).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: "Pending approvals" })).not.toBeInTheDocument();
  });

  it("refreshes_session_once_when_new_session_creation_rejects_the_saved_token", async () => {
    const user = userEvent.setup();
    let created = false;
    const createdSession = sessionSnapshot({
      threadId: "thread-refreshed-create",
      title: "Created after refresh",
      cwd: "/repo/mobile",
    });
    saveSession({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "old-token",
      sessionExpiresAt: Date.now() + 60_000,
      bridgeUrl: "http://bridge.local",
    });
    const fetchMock = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      const authorization = new Headers(init?.headers).get("Authorization");
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        if (authorization === "Bearer old-token") {
          return jsonResponse({ code: "unauthorized", error: "expired" }, 401);
        }
        created = true;
        return jsonResponse(createdSession, 201);
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse(created ? [createdSession] : []);
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/mobile" }]);
      }
      if (url === "http://bridge.local/api/session/refresh" && init?.method === "POST") {
        return jsonResponse({
          deviceId: "device-1",
          sessionToken: "new-token",
          sessionExpiresAt: Date.now() + 120_000,
        });
      }
      if (url === "http://bridge.local/api/sessions/thread-refreshed-create/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);
    await screen.findByLabelText("Workspace");
    await user.type(screen.getByLabelText("First message for new session"), "Create after refresh");
    await user.click(screen.getByRole("button", { name: "Create & send" }));

    expect(await screen.findByRole("heading", { name: "Created after refresh" })).toBeInTheDocument();
    expect(loadSession()?.sessionToken).toBe("new-token");
    expect(
      fetchMock.mock.calls.filter(
        ([input, init]) =>
          String(input) === "http://bridge.local/api/session/refresh" && init?.method === "POST",
      ),
    ).toHaveLength(1);
    expect(
      fetchMock.mock.calls.filter(
        ([input, init]) =>
          String(input) === "http://bridge.local/api/sessions" && init?.method === "POST",
      ),
    ).toHaveLength(2);
  });

  it("does_not_replay_new_session_creation_when_refreshed_health_is_not_writable", async () => {
    const user = userEvent.setup();
    let healthRequests = 0;
    let createRequests = 0;
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
      if (url === "http://bridge.local/api/health") {
        healthRequests += 1;
        return healthRequests === 1
          ? jsonResponse({ status: "ok", connectionState: "writable" })
          : jsonResponse({ status: "degraded", connectionState: "read_only" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        createRequests += 1;
        return jsonResponse({ code: "unauthorized", error: "expired" }, 401);
      }
      if (url === "http://bridge.local/api/sessions" || url === "http://bridge.local/api/approvals") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/mobile" }]);
      }
      if (url === "http://bridge.local/api/session/refresh" && init?.method === "POST") {
        return jsonResponse({
          deviceId: "device-1",
          sessionToken: "new-token",
          sessionExpiresAt: Date.now() + 120_000,
        });
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);
    await screen.findByLabelText("Workspace");
    await user.type(screen.getByLabelText("First message for new session"), "Do not replay");
    await user.click(screen.getByRole("button", { name: "Create & send" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Bridge is not writable after session refresh",
    );
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Read-only");
    expect(createRequests).toBe(1);
  });

  it("creates_only_one_session_when_the_new_session_form_is_submitted_twice_synchronously", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    let createRequests = 0;
    let resolveCreate: ((response: Response) => void) | undefined;
    const createResponse = new Promise<Response>((resolve) => {
      resolveCreate = resolve;
    });
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        createRequests += 1;
        return createResponse;
      }
      if (url === "http://bridge.local/api/sessions" || url === "http://bridge.local/api/approvals") {
        return jsonResponse([]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/mobile" }]);
      }
      if (url === "http://bridge.local/api/sessions/thread-single-create/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const newSessionButton = await screen.findByRole("button", { name: "New session" });
    await waitFor(() => expect(newSessionButton).toBeEnabled());
    await user.click(newSessionButton);
    await screen.findByLabelText("Workspace");
    await user.type(screen.getByLabelText("First message for new session"), "Submit once");
    const form = screen.getByRole("button", { name: "Create & send" }).closest("form");
    expect(form).not.toBeNull();

    act(() => {
      fireEvent.submit(form!);
      fireEvent.submit(form!);
    });

    await waitFor(() => expect(createRequests).toBe(1));
    resolveCreate?.(
      jsonResponse(
        sessionSnapshot({
          threadId: "thread-single-create",
          title: "Single create",
          cwd: "/repo/mobile",
        }),
        201,
      ),
    );
    expect(await screen.findByRole("heading", { name: "Single create" })).toBeInTheDocument();
    expect(createRequests).toBe(1);
  });

  it("keeps_new_session_failure_inside_sheet_without_replacing_current_thread", async () => {
    const user = userEvent.setup();
    stubObjectUrls();
    saveActiveSession();
    let workspaceRequests = 0;
    let resolveWorkspaceReload: ((response: Response) => void) | undefined;
    const workspaceReload = new Promise<Response>((resolve) => {
      resolveWorkspaceReload = resolve;
    });
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        return jsonResponse(
          { code: "workspace_unavailable", error: "workspace is unavailable" },
          400,
        );
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-existing",
            title: "Existing thread",
            preview: "Keep me selected",
            cwd: "/repo/existing",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        workspaceRequests += 1;
        return workspaceRequests === 1
          ? jsonResponse([{ cwd: "/repo/existing" }])
          : workspaceReload;
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
    const image = new File(
      [new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])],
      "phone.png",
      { type: "image/png" },
    );
    await user.upload(screen.getByLabelText("Choose new session image attachment"), image);
    await user.type(textarea, "This should stay in the sheet");
    await user.click(screen.getByRole("button", { name: "Create & send" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("workspace is unavailable");
    expect(screen.getByRole("alert")).not.toHaveTextContent("Pairing link expired");
    expect(textarea).toHaveValue("This should stay in the sheet");
    expect(screen.getByRole("img", { name: "phone.png" })).toBeInTheDocument();
    expect(screen.getByLabelText("Workspace")).toHaveValue("/repo/existing");
    expect(screen.getByRole("heading", { name: "Existing thread" })).toBeInTheDocument();
    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Writable");

    await act(async () => {
      resolveWorkspaceReload?.(jsonResponse([{ cwd: "/repo/existing" }]));
      await workspaceReload;
    });
    await waitFor(() => {
      expect(screen.queryByText("workspace is unavailable")).not.toBeInTheDocument();
    });
    expect(textarea).toHaveValue("This should stay in the sheet");
    expect(screen.getByRole("img", { name: "phone.png" })).toBeInTheDocument();
  });

  it("does_not_treat_a_code_less_create_error_as_an_expired_pairing_link", async () => {
    const user = userEvent.setup();
    saveActiveSession();
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions" && init?.method === "POST") {
        return jsonResponse({ error: "invalid create request" }, 400);
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-existing",
            title: "Existing thread",
            cwd: "/repo/existing",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/workspaces") {
        return jsonResponse([{ cwd: "/repo/existing" }]);
      }
      if (url === "http://bridge.local/api/sessions/thread-existing/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    await screen.findByRole("heading", { name: "Existing thread" });
    await user.click(screen.getByRole("button", { name: "New session" }));
    await user.type(screen.getByLabelText("First message for new session"), "Invalid request");
    await user.click(screen.getByRole("button", { name: "Create & send" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("invalid create request");
    expect(screen.getByRole("alert")).not.toHaveTextContent("Pairing link expired");
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
    expect(screen.getByRole("textbox", { name: "Message selected Codex session" })).toBeDisabled();
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

    const input = await screen.findByRole("textbox", { name: "Message selected Codex session" });
    await waitFor(() => expect(input).toBeEnabled());
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

  it("keeps_canonical_user_message_when_bridge_echo_arrives_after_it", () => {
    const optimistic = sessionEvent({
      id: "client-message-1",
      threadId: "thread-send",
      payload: { role: "user", text: "Deep Research 开发条件是什么?", pending: true },
      createdAt: 1_783_515_390_000,
    });
    const canonical = sessionEvent({
      id: "turn-new:item-1",
      threadId: "thread-send",
      payload: { role: "user", text: "Deep Research 开发条件是什么?" },
      createdAt: 1_783_515_390_010,
    });
    const bridgeEcho = sessionEvent({
      id: "client-message-1",
      threadId: "thread-send",
      payload: {
        role: "user",
        text: "Deep Research 开发条件是什么?",
        bridgeEcho: true,
        clientMessageId: "client-message-1",
      },
      createdAt: 1_783_515_390_020,
    });

    const afterCanonical = appendOrMergeSessionEvent([optimistic], canonical);
    const merged = appendOrMergeSessionEvent(afterCanonical, bridgeEcho);

    expect(merged).toHaveLength(1);
    expect(merged[0].id).toBe(canonical.id);
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

  it("replaces_live_turn_events_with_the_polled_snapshot_while_the_reply_is_running", () => {
    const liveUser = sessionEvent({
      id: "turn-live:user-live",
      threadId: "thread-a",
      payload: {
        role: "user",
        text: "Codex免费的token额度有多少?",
        turnId: "turn-live",
      },
      createdAt: 1_783_515_390_000,
    });
    const liveEvents = [
      sessionEvent({
        id: "turn-live:assistant-live",
        threadId: "thread-a",
        type: "message_delta",
        payload: {
          role: "assistant",
          text: "我按官方 Codex 文档核对一下。",
          turnId: "turn-live",
        },
        createdAt: 1_783_515_390_010,
      }),
      sessionEvent({
        id: "turn-live:reasoning-empty-1",
        threadId: "thread-a",
        type: "reasoning_summary_delta",
        payload: { role: "reasoning", text: "", turnId: "turn-live" },
        createdAt: 1_783_515_390_020,
      }),
      sessionEvent({
        id: "turn-live:reasoning-empty-2",
        threadId: "thread-a",
        type: "reasoning_summary_delta",
        payload: { role: "reasoning", text: "", turnId: "turn-live" },
        createdAt: 1_783_515_390_030,
      }),
    ].reduce<SessionEvent[]>(
      (events, event) => appendOrMergeSessionEvent(events, event),
      [liveUser],
    );

    const polledSnapshot = [
      sessionEvent({
        id: "turn-live:item-1",
        threadId: "thread-a",
        payload: {
          role: "user",
          text: "Codex免费的token额度有多少?",
          turnId: "turn-live",
        },
        createdAt: 1_783_515_390_000,
      }),
      sessionEvent({
        id: "turn-live:item-5",
        threadId: "thread-a",
        payload: {
          role: "assistant",
          text: "我按官方 Codex 文档核对一下。",
          turnId: "turn-live",
        },
        createdAt: 1_783_515_390_010,
      }),
    ];

    const merged = mergeIncrementalSessionEvents(liveEvents, polledSnapshot);
    const visibleGroups = groupSessionEventsForDisplay(merged);

    expect(merged.filter((event) => event.payload && typeof event.payload === "object" && !Array.isArray(event.payload) && event.payload.role === "user")).toHaveLength(1);
    expect(merged.filter((event) => event.payload && typeof event.payload === "object" && !Array.isArray(event.payload) && event.payload.role === "assistant")).toHaveLength(1);
    expect(merged.filter((event) => event.type === "reasoning_summary" || event.type === "reasoning_summary_delta")).toHaveLength(0);
    expect(visibleGroups).toHaveLength(2);
    expect(visibleGroups[1]).toMatchObject({
      kind: "assistant_turn",
      turnScope: "turn-live",
      events: [{ id: "turn-live:item-5" }],
    });
  });

  it("preserves_turn_identity_when_promoting_a_stream_delta", () => {
    const merged = appendOrMergeSessionEvent([], sessionEvent({
      id: "turn-live:assistant-live",
      type: "message_delta",
      payload: {
        role: "assistant",
        text: "Streaming",
        turnId: "turn-live",
      },
    }));

    expect(merged).toHaveLength(1);
    expect(merged[0].payload).toMatchObject({
      role: "assistant",
      text: "Streaming",
      turnId: "turn-live",
    });
  });

  it("keeps_earlier_same_turn_items_when_an_incremental_page_only_contains_the_tail", () => {
    const current = [
      sessionEvent({
        id: "turn-live:item-1",
        payload: { role: "user", text: "Keep the prompt", turnId: "turn-live" },
        createdAt: 1_783_515_390_000,
      }),
      sessionEvent({
        id: "turn-live:assistant-live",
        payload: { role: "assistant", text: "Partial", turnId: "turn-live" },
        createdAt: 1_783_515_390_010,
      }),
    ];
    const incrementalTail = [
      sessionEvent({
        id: "turn-live:item-5",
        payload: { role: "assistant", text: "Partial answer", turnId: "turn-live" },
        createdAt: 1_783_515_390_010,
      }),
    ];

    const merged = mergeIncrementalSessionEvents(current, incrementalTail);

    expect(merged.map((event) => event.id)).toEqual([
      "turn-live:item-1",
      "turn-live:item-5",
    ]);
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

  it("collapses_long_approval_content_until_the_user_expands_it", async () => {
    const user = userEvent.setup();
    mockApprovalDetailOverflow();
    saveActiveSession();
    const longCommand = Array.from(
      { length: 8 },
      (_, index) => `step-${index + 1} --workspace /Users/example/project --check all`,
    ).join("\n");
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-long-approval",
            title: "Long approval",
            status: "waiting_for_approval",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([
          {
            id: "approval-long",
            threadId: "thread-long-approval",
            kind: "command",
            title: "Run long command",
            detail: longCommand,
            createdAt: 1_783_584_000_000,
          },
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-long-approval/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    const expandButton = await screen.findByRole("button", { name: "Expand Run long command" });
    const approval = expandButton.closest("article");
    expect(approval).not.toBeNull();
    expect(expandButton).toHaveAttribute("aria-expanded", "false");
    const detail = (approval as HTMLElement).querySelector(".approval-detail");
    expect(detail).not.toBeNull();
    expect(detail).toHaveTextContent("step-1 --workspace /Users/example/project --check all");
    expect(detail).toHaveClass("approval-detail");
    expect(within(approval as HTMLElement).getByRole("button", { name: "Reject Run long command" })).toBeVisible();
    expect(within(approval as HTMLElement).getByRole("button", { name: "Approve Run long command" })).toBeVisible();

    await user.click(expandButton);

    expect(expandButton).toHaveAttribute("aria-expanded", "true");
    expect(detail).toHaveClass("expanded");
    expect(within(approval as HTMLElement).getByRole("button", { name: "Collapse Run long command" })).toBeInTheDocument();
  });

  it("offers_expansion_when_short_approval_text_wraps_beyond_three_visual_lines", async () => {
    mockApprovalDetailOverflow();
    saveActiveSession();
    const wrappedCommand = "npm run verify -- --workspace packages/mobile --configuration production";
    expect(wrappedCommand.length).toBeLessThan(140);
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
      const url = String(input);
      if (url === "http://bridge.local/api/health") {
        return jsonResponse({ status: "ok", connectionState: "writable" });
      }
      if (url === "http://bridge.local/api/sessions") {
        return jsonResponse([
          sessionSnapshot({
            threadId: "thread-wrapped-approval",
            title: "Wrapped approval",
            status: "waiting_for_approval",
          }),
        ]);
      }
      if (url === "http://bridge.local/api/approvals") {
        return jsonResponse([
          {
            id: "approval-wrapped",
            threadId: "thread-wrapped-approval",
            kind: "command",
            title: "Run wrapped command",
            detail: wrappedCommand,
            createdAt: 1_783_584_000_000,
          },
        ]);
      }
      if (url === "http://bridge.local/api/sessions/thread-wrapped-approval/events") {
        return jsonResponse([]);
      }
      return jsonResponse({});
    });

    render(<App />);

    expect(
      await screen.findByRole("button", { name: "Expand Run wrapped command" }),
    ).toBeInTheDocument();
  });
});

function mockApprovalDetailOverflow() {
  vi.spyOn(HTMLElement.prototype, "scrollHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("approval-detail") ? 80 : 0;
  });
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (this: HTMLElement) {
    return this.classList.contains("approval-detail") ? 40 : 0;
  });
}

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

function notificationSettingsResponse(
  overrides: Partial<{
    enabled: boolean;
    soundEnabled: boolean;
    vibrationEnabled: boolean;
    foregroundVibration: boolean;
    fixedHttps: boolean;
    deliveryMode: "foreground_only" | "web_push";
    systemNotifications: boolean;
    subscriptionState: "unavailable" | "not_enabled" | "active" | "needs_repair";
  }> = {},
) {
  return {
    settings: {
      enabled: overrides.enabled ?? false,
      alertKinds: {
        completed: true,
        approvalRequired: true,
        inputRequired: true,
        error: true,
      },
      soundEnabled: overrides.soundEnabled ?? true,
      vibrationEnabled: overrides.vibrationEnabled ?? true,
    },
    capabilities: {
      deliveryMode: overrides.deliveryMode ?? "foreground_only",
      fixedHttps: overrides.fixedHttps ?? false,
      systemNotifications: overrides.systemNotifications ?? false,
      foregroundSound: true,
      foregroundVibration: overrides.foregroundVibration ?? false,
      vibrationControlledBySystem: false,
    },
    subscriptionState: overrides.subscriptionState ?? "unavailable",
  };
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

function sessionButtonNames(container: HTMLElement): string[] {
  return within(container)
    .getAllByRole("button")
    .filter((button) => button.classList.contains("session-row-select"))
    .map((button) => button.getAttribute("aria-label") ?? "");
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

class MockServiceWorkerContainer extends EventTarget {
  readonly ready: Promise<ServiceWorkerRegistration>;

  constructor(pushManager?: Pick<PushManager, "getSubscription" | "subscribe">) {
    super();
    this.ready = Promise.resolve({
      pushManager: pushManager ?? {
        getSubscription: vi.fn(async () => null),
        subscribe: vi.fn(),
      },
    } as unknown as ServiceWorkerRegistration);
  }

  emit(data: unknown) {
    this.dispatchEvent(new MessageEvent("message", { data }));
  }
}

function stubFixedPushEnvironment() {
  let permission: NotificationPermission = "default";
  let currentSubscription: PushSubscription | null = null;
  const requestPermission = vi.fn(async () => {
    permission = "granted";
    return permission;
  });
  const subscription = {
    endpoint: "https://push.example/device-1",
    expirationTime: null,
    options: {},
    getKey: vi.fn(),
    toJSON: vi.fn(() => ({
      endpoint: "https://push.example/device-1",
      expirationTime: null,
      keys: { p256dh: "client-public-key", auth: "client-auth" },
    })),
    unsubscribe: vi.fn(async () => {
      currentSubscription = null;
      return true;
    }),
  } as unknown as PushSubscription;
  const getSubscription = vi.fn(async () => currentSubscription);
  const subscribe = vi.fn(async () => {
    currentSubscription = subscription;
    return subscription;
  });
  const serviceWorker = new MockServiceWorkerContainer({ getSubscription, subscribe });
  stubNavigator({ serviceWorker, vibrate: vi.fn() });
  vi.stubGlobal("Notification", {
    get permission() {
      return permission;
    },
    requestPermission,
  });
  vi.stubGlobal("PushManager", class PushManager {});
  vi.stubGlobal("isSecureContext", true);
  return { requestPermission, subscribe };
}

function stubNavigator({
  serviceWorker,
  vibrate,
}: {
  serviceWorker: MockServiceWorkerContainer;
  vibrate: ReturnType<typeof vi.fn>;
}) {
  vi.stubGlobal("navigator", {
    userAgent: "Mozilla/5.0 (Linux; Android 15)",
    platform: "Linux armv8l",
    maxTouchPoints: 5,
    serviceWorker,
    vibrate,
  });
}
