const PREFIX = "codex.mobilePwa.notificationOnboarding.v1";

export function onboardingStorageKey(deviceId: string): string {
  return `${PREFIX}:${deviceId}`;
}

export function dismissNotificationOnboarding(deviceId: string): void {
  localStorage.setItem(onboardingStorageKey(deviceId), "dismissed");
}

export function hasDismissedNotificationOnboarding(deviceId: string): boolean {
  return localStorage.getItem(onboardingStorageKey(deviceId)) === "dismissed";
}
