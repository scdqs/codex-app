export interface PushCapabilities {
  fixedHttps: boolean;
  secureContext: boolean;
  serviceWorker: boolean;
  pushManager: boolean;
  notificationApi: boolean;
  isIos: boolean;
  standalone: boolean;
}

export interface ForegroundCapabilities extends PushCapabilities {
  deliveryMode: "foreground_only";
  foregroundSound: boolean;
  foregroundVibration: boolean;
  vibrationControlledBySystem: boolean;
  lockScreenMessage: string;
}

export function detectForegroundCapabilities(input: {
  fixedHttps: boolean;
  hasAudioContext: boolean;
  hasVibrate: boolean;
  secureContext: boolean;
  serviceWorker: boolean;
  pushManager: boolean;
  notificationApi: boolean;
  isIos: boolean;
  standalone: boolean;
}): ForegroundCapabilities {
  return {
    deliveryMode: "foreground_only",
    fixedHttps: input.fixedHttps,
    secureContext: input.secureContext,
    serviceWorker: input.serviceWorker,
    pushManager: input.pushManager,
    notificationApi: input.notificationApi,
    isIos: input.isIos,
    standalone: input.standalone,
    foregroundSound: input.hasAudioContext,
    foregroundVibration: input.hasVibrate,
    vibrationControlledBySystem: input.isIos,
    lockScreenMessage:
      input.fixedHttps && input.isIos && !input.standalone
        ? "Add this app to the Home Screen to enable lock-screen alerts"
        : input.fixedHttps
          ? "Fixed address is ready; enable Web Push for lock-screen alerts"
          : "Lock-screen alerts require a fixed HTTPS address",
  };
}

export function browserForegroundCapabilities(fixedHttps: boolean): ForegroundCapabilities {
  const browserWindow = window;
  const browserNavigator = navigator as Navigator & { standalone?: boolean };
  const isIos = isIosBrowser(browserNavigator);
  const standalone =
    browserNavigator.standalone === true ||
    browserWindow.matchMedia?.("(display-mode: standalone)").matches === true;
  return detectForegroundCapabilities({
    fixedHttps,
    hasAudioContext: "AudioContext" in browserWindow || "webkitAudioContext" in browserWindow,
    hasVibrate: typeof browserNavigator.vibrate === "function",
    secureContext: browserWindow.isSecureContext,
    serviceWorker: "serviceWorker" in browserNavigator,
    pushManager: "PushManager" in browserWindow,
    notificationApi: "Notification" in browserWindow,
    isIos,
    standalone,
  });
}

function isIosBrowser(browserNavigator: Navigator): boolean {
  return (
    /iPad|iPhone|iPod/i.test(browserNavigator.userAgent) ||
    (browserNavigator.platform === "MacIntel" && browserNavigator.maxTouchPoints > 1)
  );
}
