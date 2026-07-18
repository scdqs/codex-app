import { describe, expect, it, vi } from "vitest";
import type { PushCapabilities } from "./capabilities";
import {
  PushSubscriptionController,
  type BrowserPushSubscription,
  type PushSubscriptionPorts,
} from "./push-subscription-controller";

describe("PushSubscriptionController", () => {
  it("does_not_request_permission_or_subscribe_in_foreground_only_mode", async () => {
    const ports = pushPorts({ permission: "default" });
    const controller = new PushSubscriptionController(ports);

    await expect(controller.enable(capabilities({ fixedHttps: false }))).rejects.toMatchObject({
      code: "push_unavailable",
    });

    expect(ports.requestPermission).not.toHaveBeenCalled();
    expect(ports.subscribe).not.toHaveBeenCalled();
  });

  it("requires_home_screen_install_on_ios_before_permission_prompt", async () => {
    const ports = pushPorts({ permission: "default" });
    const controller = new PushSubscriptionController(ports);

    await expect(
      controller.enable(capabilities({ isIos: true, standalone: false })),
    ).rejects.toMatchObject({ code: "ios_install_required" });
    expect(ports.requestPermission).not.toHaveBeenCalled();
  });

  it("does_not_reprompt_when_notification_permission_is_denied", async () => {
    const ports = pushPorts({ permission: "denied" });
    const controller = new PushSubscriptionController(ports);

    await expect(controller.enable(fixedAndroid())).rejects.toMatchObject({
      code: "permission_denied",
    });
    expect(ports.requestPermission).not.toHaveBeenCalled();
  });

  it("requests_permission_subscribes_and_registers_with_the_bridge", async () => {
    const ports = pushPorts({ permission: "default", requestedPermission: "granted" });
    const controller = new PushSubscriptionController(ports);

    await controller.enable(fixedAndroid());

    expect(ports.getPublicKey).toHaveBeenCalled();
    expect(ports.subscribe).toHaveBeenCalledWith({
      userVisibleOnly: true,
      applicationServerKey: expect.any(Uint8Array),
    });
    expect(ports.saveSubscription).toHaveBeenCalledWith({
      origin: "https://codex.example.com",
      subscription: expect.objectContaining({ endpoint: "https://push.example/fresh" }),
    });
  });

  it("reuses_an_existing_browser_subscription_before_registering_it", async () => {
    const existing = fakeSubscription("existing");
    const ports = pushPorts({ permission: "granted", existing });
    const controller = new PushSubscriptionController(ports);

    await controller.enable(fixedAndroid());

    expect(ports.subscribe).not.toHaveBeenCalled();
    expect(ports.saveSubscription).toHaveBeenCalledWith({
      origin: "https://codex.example.com",
      subscription: existing.toJSON(),
    });
  });

  it("repair_unsubscribes_stale_browser_record_before_resubscribing", async () => {
    const order: string[] = [];
    const stale = fakeSubscription("stale", async () => {
      order.push("unsubscribe-browser");
      return true;
    });
    const fresh = fakeSubscription("fresh");
    const ports = pushPorts({ permission: "granted" });
    ports.deleteServerSubscription.mockImplementation(async () => {
      order.push("delete-server");
    });
    ports.getSubscription
      .mockImplementationOnce(async () => stale)
      .mockImplementationOnce(async () => null);
    ports.subscribe.mockImplementation(async () => {
      order.push("subscribe-browser");
      return fresh;
    });
    ports.saveSubscription.mockImplementation(async () => {
      order.push("save-server");
    });
    const controller = new PushSubscriptionController(ports);

    await controller.repair(fixedAndroid());

    expect(order).toEqual([
      "delete-server",
      "unsubscribe-browser",
      "subscribe-browser",
      "save-server",
    ]);
  });

  it("disable_attempts_server_and_browser_cleanup_when_one_side_fails", async () => {
    const stale = fakeSubscription("stale");
    const ports = pushPorts({ permission: "granted", existing: stale });
    ports.deleteServerSubscription.mockRejectedValue(new Error("bridge unavailable"));
    const controller = new PushSubscriptionController(ports);

    await expect(controller.disable()).rejects.toMatchObject({ code: "disable_failed" });

    expect(ports.deleteServerSubscription).toHaveBeenCalledTimes(1);
    expect(stale.unsubscribe).toHaveBeenCalledTimes(1);
  });

  it("reports_browser_and_server_mismatches_as_needing_repair", async () => {
    const ports = pushPorts({ permission: "granted", existing: null });
    const controller = new PushSubscriptionController(ports);

    await expect(controller.inspect(fixedAndroid(), "active")).resolves.toBe("needs_repair");
    ports.getSubscription.mockResolvedValue(fakeSubscription("active"));
    await expect(controller.inspect(fixedAndroid(), "active")).resolves.toBe("active");
  });
});

function fixedAndroid(): PushCapabilities {
  return capabilities();
}

function capabilities(overrides: Partial<PushCapabilities> = {}): PushCapabilities {
  return {
    fixedHttps: true,
    secureContext: true,
    serviceWorker: true,
    pushManager: true,
    notificationApi: true,
    isIos: false,
    standalone: true,
    ...overrides,
  };
}

function fakeSubscription(
  id: string,
  onUnsubscribe: () => Promise<boolean> = async () => true,
): BrowserPushSubscription {
  return {
    toJSON: vi.fn(() => ({
      endpoint: `https://push.example/${id}`,
      expirationTime: null,
      keys: { p256dh: "client-public-key", auth: "client-auth" },
    })),
    unsubscribe: vi.fn(onUnsubscribe),
  };
}

function pushPorts(options: {
  permission?: NotificationPermission;
  requestedPermission?: NotificationPermission;
  existing?: BrowserPushSubscription | null;
} = {}) {
  let permission = options.permission ?? "granted";
  const fresh = fakeSubscription("fresh");
  return {
    permission: vi.fn(() => permission),
    requestPermission: vi.fn(async () => {
      permission = options.requestedPermission ?? "granted";
      return permission;
    }),
    getPublicKey: vi.fn(async () => "BBDh6d4q3G2c9vl9IK2JvlfqubT4Lpi0JYYA-mock-public-key"),
    getSubscription: vi.fn(async () => options.existing ?? null),
    subscribe: vi.fn(async () => fresh),
    saveSubscription: vi.fn(async () => undefined),
    deleteServerSubscription: vi.fn(async () => undefined),
    origin: vi.fn(() => "https://codex.example.com"),
  } satisfies PushSubscriptionPorts & Record<string, ReturnType<typeof vi.fn>>;
}
