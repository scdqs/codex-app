const SESSION_STORAGE_KEY = "codex.mobilePwa.deviceSession.v1";

export interface DeviceSession {
  deviceId: string;
  deviceSecret: string;
  displayName: string;
  sessionToken?: string;
  sessionExpiresAt?: string;
  bridgeUrl?: string;
}

export function saveSession(session: DeviceSession): void {
  localStorage.setItem(SESSION_STORAGE_KEY, JSON.stringify(session));
}

export function loadSession(): DeviceSession | null {
  const raw = localStorage.getItem(SESSION_STORAGE_KEY);
  if (!raw) {
    return null;
  }

  try {
    const value = JSON.parse(raw) as Partial<DeviceSession>;
    if (!value.deviceId || !value.deviceSecret || !value.displayName) {
      return null;
    }
    return value as DeviceSession;
  } catch {
    return null;
  }
}

export function clearSession(): void {
  localStorage.removeItem(SESSION_STORAGE_KEY);
}

export function createDeviceSession(input: {
  bridgeUrl?: string;
  displayName?: string;
  existing?: DeviceSession | null;
}): DeviceSession {
  return {
    deviceId: input.existing?.deviceId ?? randomId(),
    deviceSecret: input.existing?.deviceSecret ?? randomId(),
    displayName: input.displayName ?? input.existing?.displayName ?? defaultDisplayName(),
    sessionToken: input.existing?.sessionToken,
    sessionExpiresAt: input.existing?.sessionExpiresAt,
    bridgeUrl: input.bridgeUrl ?? input.existing?.bridgeUrl,
  };
}

function defaultDisplayName(): string {
  if (typeof navigator !== "undefined" && navigator.userAgent) {
    return "Mobile PWA";
  }
  return "Test device";
}

function randomId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }

  return `id-${Math.random().toString(36).slice(2)}-${Date.now().toString(36)}`;
}
