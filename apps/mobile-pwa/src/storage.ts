const SESSION_STORAGE_KEY = "codex.mobilePwa.deviceSession.v1";

export interface DeviceSession {
  deviceId: string;
  deviceSecret: string;
  displayName: string;
  sessionToken: string;
  sessionExpiresAt: number;
  bridgeUrl: string;
}

export type DeviceIdentity = Pick<DeviceSession, "deviceId" | "deviceSecret" | "displayName"> & {
  bridgeUrl?: string;
};

export function saveSession(session: DeviceSession): void {
  localStorage.setItem(SESSION_STORAGE_KEY, JSON.stringify(session));
}

export function loadSession(): DeviceSession | null {
  const raw = localStorage.getItem(SESSION_STORAGE_KEY);
  if (!raw) {
    return null;
  }

  try {
    const value = JSON.parse(raw) as unknown;
    if (!isDeviceSession(value)) {
      return null;
    }
    return value;
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
}): DeviceIdentity {
  return {
    deviceId: input.existing?.deviceId ?? randomId(),
    deviceSecret: input.existing?.deviceSecret ?? randomId(),
    displayName: input.displayName ?? input.existing?.displayName ?? defaultDisplayName(),
    bridgeUrl: input.bridgeUrl ?? input.existing?.bridgeUrl,
  };
}

function isDeviceSession(value: unknown): value is DeviceSession {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const session = value as Record<string, unknown>;
  return (
    typeof session.deviceId === "string" &&
    typeof session.deviceSecret === "string" &&
    typeof session.displayName === "string" &&
    typeof session.sessionToken === "string" &&
    typeof session.sessionExpiresAt === "number" &&
    Number.isFinite(session.sessionExpiresAt) &&
    typeof session.bridgeUrl === "string"
  );
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
