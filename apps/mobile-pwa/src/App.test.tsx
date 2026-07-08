import { afterEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";
import { clearSession, loadSession, saveSession } from "./storage";

describe("App", () => {
  afterEach(() => {
    vi.restoreAllMocks();
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
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
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
    expect(globalThis.fetch).toHaveBeenCalledTimes(1);
    expect(globalThis.fetch).toHaveBeenCalledWith("http://bridge.local/api/health", {
      headers: { Authorization: "Bearer session-1" },
    });
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
    expect(globalThis.fetch).toHaveBeenCalledTimes(2);
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
    expect(globalThis.fetch).toHaveBeenCalledTimes(3);
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
});
