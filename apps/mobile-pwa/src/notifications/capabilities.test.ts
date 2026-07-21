import { describe, expect, it } from "vitest";
import { detectForegroundCapabilities } from "./capabilities";

describe("notification capabilities", () => {
  it("reports foreground-only and unsupported vibration honestly", () => {
    expect(
      detectForegroundCapabilities({
        fixedHttps: false,
        hasAudioContext: true,
        hasVibrate: false,
        secureContext: true,
        serviceWorker: true,
        pushManager: true,
        notificationApi: true,
        isIos: false,
        standalone: false,
      }),
    ).toEqual({
      deliveryMode: "foreground_only",
      fixedHttps: false,
      foregroundSound: true,
      foregroundVibration: false,
      vibrationControlledBySystem: false,
      lockScreenMessage: "Lock-screen alerts require a fixed HTTPS address",
      secureContext: true,
      serviceWorker: true,
      pushManager: true,
      notificationApi: true,
      isIos: false,
      standalone: false,
    });
  });

  it("reports_ios_home_screen_install_and_system_control", () => {
    expect(
      detectForegroundCapabilities({
        fixedHttps: true,
        hasAudioContext: true,
        hasVibrate: false,
        secureContext: true,
        serviceWorker: true,
        pushManager: true,
        notificationApi: true,
        isIos: true,
        standalone: false,
      }),
    ).toMatchObject({
      isIos: true,
      standalone: false,
      vibrationControlledBySystem: true,
      lockScreenMessage: "Add this app to the Home Screen to enable lock-screen alerts",
    });
  });
});
