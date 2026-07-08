import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ApiValidationError,
  completePairing,
  connectWebSocket,
  fetchAssetBlob,
  readPairingPayloadFromUrl,
} from "./api";
import { clearSession, loadSession, saveSession } from "./storage";

describe("pairing API helpers", () => {
  const expiresAt = 1_783_584_000_000;

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
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

  it("rejects_malformed_pairing_response", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(JSON.stringify({ deviceId: "device-1" }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );

    await expect(
      completePairing("http://bridge.local", {
        pairingToken: "pair-1",
        deviceId: "device-1",
        displayName: "Damon Phone",
        deviceSecret: "secret-1",
      }),
    ).rejects.toBeInstanceOf(ApiValidationError);
    expect(loadSession()).toBeNull();
  });

  it("connectWebSocket_builds_ws_url_from_http_bridge", () => {
    const urls: string[] = [];
    vi.stubGlobal(
      "WebSocket",
      class {
        constructor(url: string) {
          urls.push(url);
        }
      },
    );

    connectWebSocket("http://bridge.local:4545", "token with spaces");

    expect(urls).toEqual(["ws://bridge.local:4545/ws?token=token+with+spaces"]);
  });

  it("connectWebSocket_builds_wss_url_from_https_bridge", () => {
    const urls: string[] = [];
    vi.stubGlobal(
      "WebSocket",
      class {
        constructor(url: string) {
          urls.push(url);
        }
      },
    );

    connectWebSocket("https://bridge.local", "session-1");

    expect(urls).toEqual(["wss://bridge.local/ws?token=session-1"]);
  });

  it.each(["https://attacker.example/x", "//attacker.example/x"])(
    "fetchAssetBlob_rejects_off_origin_asset_source_without_fetch",
    async (src) => {
      const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(jsonResponse({}));

      await expect(fetchAssetBlob("http://bridge.local", "session-1", src)).rejects.toMatchObject({
        status: 400,
        message: "Invalid asset source",
      });
      expect(fetchMock).not.toHaveBeenCalled();
    },
  );

  it("fetchAssetBlob_rejects_non_asset_paths_without_fetch", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(jsonResponse({}));

    await expect(
      fetchAssetBlob("http://bridge.local", "session-1", "/api/sessions/thread/events"),
    ).rejects.toMatchObject({
      status: 400,
      message: "Invalid asset source",
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it.each([
    "/api/assets/../sessions",
    "/api/assets/local-image/../../sessions",
    "/api/assets/%2e%2e/sessions",
  ])("fetchAssetBlob_rejects_traversal_asset_paths_without_fetch", async (src) => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(jsonResponse({}));

    await expect(fetchAssetBlob("http://bridge.local", "session-1", src)).rejects.toMatchObject({
      status: 400,
      message: "Invalid asset source",
    });
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("fetchAssetBlob_rejects_non_image_content_type", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response("not an image", {
        status: 200,
        headers: { "Content-Type": "text/plain" },
      }),
    );

    await expect(
      fetchAssetBlob("http://bridge.local", "session-1", "/api/assets/local-image/asset-1"),
    ).rejects.toMatchObject({
      status: 200,
      message: "Asset response is not an image",
    });
  });

  it("fetchAssetBlob_accepts_asset_paths_and_sends_authorization", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValueOnce(
      new Response(new Blob(["png"], { type: "image/png" }), {
        status: 200,
        headers: { "Content-Type": "image/png" },
      }),
    );

    const blob = await fetchAssetBlob("http://bridge.local", "session-1", "/api/assets/local-image/asset-1");

    expect(blob.type).toBe("image/png");
    expect(fetchMock).toHaveBeenCalledWith("http://bridge.local/api/assets/local-image/asset-1", {
      headers: { Authorization: "Bearer session-1" },
    });
  });
});

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
