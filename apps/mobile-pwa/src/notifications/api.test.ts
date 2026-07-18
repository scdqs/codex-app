import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  deletePushSubscription,
  getNotificationSettings,
  getPushPublicKey,
  putNotificationSettings,
  savePushSubscription,
  type NotificationSettingsInput,
} from "./api";
import type { DeviceSession } from "../storage";

const session: DeviceSession = {
  deviceId: "phone-1",
  deviceSecret: "secret",
  displayName: "Phone",
  sessionToken: "session-token",
  sessionExpiresAt: Date.now() + 60_000,
  bridgeUrl: "https://codex.example.com",
};

const settings: NotificationSettingsInput = {
  enabled: true,
  alertKinds: {
    completed: true,
    approvalRequired: false,
    inputRequired: true,
    error: true,
  },
  soundEnabled: false,
  vibrationEnabled: true,
};

describe("notification settings API", () => {
  beforeEach(() => vi.restoreAllMocks());

  it("sends bearer auth and the complete settings document", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(jsonResponse(responseBody()));

    await putNotificationSettings(session, settings);

    expect(fetchMock).toHaveBeenCalledWith(
      "https://codex.example.com/api/notification-settings",
      expect.objectContaining({
        method: "PUT",
        headers: {
          Authorization: "Bearer session-token",
          "Content-Type": "application/json",
        },
        body: JSON.stringify(settings),
      }),
    );
  });

  it("strictly rejects malformed server responses", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(
      jsonResponse({ ...responseBody(), settings: { enabled: true } }),
    );

    await expect(getNotificationSettings(session)).rejects.toThrow(
      "Invalid notification settings response",
    );
  });

  it("registers and deletes only the current browser push subscription", async () => {
    const fetchMock = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(new Response(null, { status: 201 }))
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    const subscription: PushSubscriptionJSON = {
      endpoint: "https://push.example/device-1",
      expirationTime: null,
      keys: { p256dh: "client-public-key", auth: "client-auth" },
    };

    await savePushSubscription(session, "https://codex.example.com", subscription);
    await deletePushSubscription(session);

    expect(fetchMock).toHaveBeenNthCalledWith(
      1,
      "https://codex.example.com/api/push/subscription",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          origin: "https://codex.example.com",
          endpoint: "https://push.example/device-1",
          keys: { p256dh: "client-public-key", auth: "client-auth" },
        }),
      }),
    );
    expect(fetchMock).toHaveBeenNthCalledWith(
      2,
      "https://codex.example.com/api/push/subscription",
      expect.objectContaining({ method: "DELETE" }),
    );
  });

  it("rejects incomplete browser subscriptions before sending them", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch");

    await expect(
      savePushSubscription(session, "https://codex.example.com", {
        endpoint: "https://push.example/device-1",
        expirationTime: null,
      }),
    ).rejects.toThrow("Invalid PushSubscription");

    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("strictly parses the VAPID public key response", async () => {
    vi.spyOn(globalThis, "fetch").mockResolvedValue(jsonResponse({ publicKey: "vapid-public" }));

    await expect(getPushPublicKey(session)).resolves.toBe("vapid-public");
  });
});

function responseBody() {
  return {
    settings,
    capabilities: {
      deliveryMode: "foreground_only",
      fixedHttps: true,
      systemNotifications: false,
      foregroundSound: true,
      foregroundVibration: true,
      vibrationControlledBySystem: false,
    },
    subscriptionState: "unavailable",
  };
}

function jsonResponse(value: unknown): Response {
  return new Response(JSON.stringify(value), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}
