import { afterEach, describe, expect, it, vi } from "vitest";
import { completePairing, readPairingPayloadFromUrl } from "./api";
import { clearSession, loadSession, saveSession } from "./storage";

describe("pairing API helpers", () => {
  const expiresAt = 1_783_584_000_000;

  afterEach(() => {
    vi.restoreAllMocks();
    clearSession();
  });

  it("reads_pairing_payload_from_url", () => {
    const payload = readPairingPayloadFromUrl(
      "https://phone.local/pair?pairingToken=pair-123&bridgeUrl=http%3A%2F%2F192.168.1.8%3A4545&deviceName=Damon%20Phone",
    );

    expect(payload).toEqual({
      pairingToken: "pair-123",
      bridgeUrl: "http://192.168.1.8:4545",
      displayName: "Damon Phone",
    });
    expect(readPairingPayloadFromUrl("https://phone.local/")).toBeNull();
  });

  it("stores_device_session_after_pairing", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          deviceId: "device-1",
          sessionToken: "session-1",
          sessionExpiresAt: expiresAt,
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
    );

    const response = await completePairing("http://bridge.local", {
      pairingToken: "pair-1",
      deviceId: "device-1",
      displayName: "Damon Phone",
      deviceSecret: "secret-1",
    });

    saveSession({
      deviceId: response.deviceId,
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: response.sessionToken,
      sessionExpiresAt: response.sessionExpiresAt,
      bridgeUrl: "http://bridge.local",
    });

    expect(loadSession()).toMatchObject({
      deviceId: "device-1",
      deviceSecret: "secret-1",
      displayName: "Damon Phone",
      sessionToken: "session-1",
      sessionExpiresAt: expiresAt,
      bridgeUrl: "http://bridge.local",
    });
  });

  it("rejects_incomplete_or_wrongly_typed_stored_sessions", () => {
    localStorage.setItem(
      "codex.mobilePwa.deviceSession.v1",
      JSON.stringify({
        deviceId: "device-1",
        deviceSecret: "secret-1",
        displayName: "Damon Phone",
        sessionToken: "session-1",
        sessionExpiresAt: expiresAt,
      }),
    );
    expect(loadSession()).toBeNull();

    localStorage.setItem(
      "codex.mobilePwa.deviceSession.v1",
      JSON.stringify({
        deviceId: "device-1",
        deviceSecret: "secret-1",
        displayName: "Damon Phone",
        sessionToken: "session-1",
        sessionExpiresAt: "2026-07-09T00:00:00.000Z",
        bridgeUrl: "http://bridge.local",
      }),
    );
    expect(loadSession()).toBeNull();
  });
});
