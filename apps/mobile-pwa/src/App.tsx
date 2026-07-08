import { FormEvent, useEffect, useMemo, useState } from "react";
import {
  AlertTriangle,
  Check,
  ChevronRight,
  CircleDot,
  Clock3,
  Command,
  FilePenLine,
  Send,
  ShieldAlert,
  TerminalSquare,
  X,
} from "lucide-react";
import type { ApprovalKind, ApprovalRequest, SessionEvent, SessionSnapshot } from "./protocol";
import {
  ApiError,
  completePairing,
  getHealth,
  readPairingPayloadFromUrl,
  refreshSession,
  type HealthResponse,
  type PairingPayload,
} from "./api";
import { createDeviceSession, loadSession, saveSession, type DeviceSession } from "./storage";

const sessions: SessionSnapshot[] = [
  {
    threadId: "thread-mobile-bridge",
    title: "Mobile bridge MVP",
    cwd: "/Users/damon/Documents/my_ai/codex-app",
    modelProvider: "OpenAI",
    preview: "Scaffold PWA workbench and keep sidecar protocol aligned.",
    updatedAt: 1_783_515_380_000,
    status: "waiting_for_approval",
    pendingApprovalIds: ["approval-install"],
  },
  {
    threadId: "thread-sidecar",
    title: "Bridge sidecar API",
    cwd: "/Users/damon/Documents/my_ai/codex-app",
    modelProvider: "OpenAI",
    preview: "HTTP health, pairing, and WebSocket replay are ready for PWA wiring.",
    updatedAt: 1_783_514_520_000,
    status: "running",
    pendingApprovalIds: [],
  },
  {
    threadId: "thread-docs",
    title: "MVP plan notes",
    preview: "Next tasks add pairing, connection state, and live session streams.",
    updatedAt: 1_783_510_800_000,
    status: "idle",
    pendingApprovalIds: [],
  },
];

const approvals: ApprovalRequest[] = [
  {
    id: "approval-install",
    threadId: "thread-mobile-bridge",
    kind: "command",
    title: "Run npm install",
    detail: "cd apps/mobile-pwa && npm install",
    riskHint: "Writes node_modules and package-lock.json in the PWA package.",
    createdAt: 1_783_515_360_000,
  },
  {
    id: "approval-build",
    threadId: "thread-mobile-bridge",
    kind: "file_edit",
    title: "Create PWA scaffold",
    detail: "Add Vite React files under apps/mobile-pwa.",
    createdAt: 1_783_515_080_000,
  },
];

const events: SessionEvent[] = [
  {
    id: "event-1",
    threadId: "thread-mobile-bridge",
    type: "message",
    payload: { role: "user", text: "Implement Task 6 only. Do not touch Rust." },
    createdAt: 1_783_515_000_000,
  },
  {
    id: "event-2",
    threadId: "thread-mobile-bridge",
    type: "tool_call",
    payload: { tool: "read_memory", text: "Loaded project constraints and current scope." },
    createdAt: 1_783_515_120_000,
  },
  {
    id: "event-3",
    threadId: "thread-mobile-bridge",
    type: "approval_requested",
    payload: { text: "Waiting for dependency install approval." },
    createdAt: 1_783_515_360_000,
  },
];

type ConnectionLabel =
  | "Unpaired"
  | "Pairing"
  | "Connected"
  | "Codex not running"
  | "Inject failed"
  | "Read-only"
  | "Writable"
  | "Connection error";

interface ConnectionViewState {
  label: ConnectionLabel;
  detail?: string;
}

const pairingAttempts = new Map<string, Promise<DeviceSession>>();

function App() {
  const [selectedThreadId, setSelectedThreadId] = useState(sessions[0].threadId);
  const [draft, setDraft] = useState("");
  const [connection, setConnection] = useState<ConnectionViewState>({ label: "Unpaired" });
  const selectedSession = sessions.find((session) => session.threadId === selectedThreadId) ?? sessions[0];
  const selectedApprovals = approvals.filter((approval) => approval.threadId === selectedSession.threadId);
  const selectedEvents = events.filter((event) => event.threadId === selectedSession.threadId);
  const pendingCount = approvals.length;

  const statusText = useMemo(() => {
    if (pendingCount > 0) {
      return `${pendingCount} pending`;
    }
    return "Writable";
  }, [pendingCount]);

  useEffect(() => {
    let cancelled = false;

    async function loadConnection() {
      const pairingPayload = readPairingPayloadFromUrl(window.location.href);
      const savedSession = loadSession();
      const shouldCompletePairing =
        pairingPayload !== null && (!savedSession || isExpired(savedSession.sessionExpiresAt));
      const bridgeUrl = shouldCompletePairing
        ? pairingPayload.bridgeUrl ?? savedSession?.bridgeUrl ?? window.location.origin
        : savedSession?.bridgeUrl ?? window.location.origin;

      try {
        if (shouldCompletePairing) {
          setConnection({ label: "Pairing" });
          const pairedSession = await completePairingOnce(bridgeUrl, pairingPayload, savedSession);
          clearPairingParamsFromUrl();
          const health = await getHealth(bridgeUrl, pairedSession.sessionToken);
          if (!cancelled) {
            setConnection(mapHealthToConnection(health));
          }
          return;
        }

        if (!savedSession) {
          setConnection({ label: "Unpaired" });
          return;
        }

        if (isExpired(savedSession.sessionExpiresAt)) {
          setConnection({ label: "Pairing", detail: "Refreshing session" });
          const refreshed = await refreshSession(bridgeUrl, savedSession);
          const nextSession = {
            ...savedSession,
            deviceId: refreshed.deviceId,
            sessionToken: refreshed.sessionToken,
            sessionExpiresAt: refreshed.sessionExpiresAt,
            bridgeUrl,
          };
          saveSession(nextSession);
          const health = await getHealth(bridgeUrl, nextSession.sessionToken);
          if (!cancelled) {
            setConnection(mapHealthToConnection(health));
          }
          return;
        }

        setConnection({ label: "Connected" });
        if (pairingPayload) {
          clearPairingParamsFromUrl();
        }
        const health = await getHealthWithRefresh(bridgeUrl, savedSession);
        if (!cancelled) {
          setConnection(mapHealthToConnection(health));
        }
      } catch (error) {
        if (cancelled) {
          return;
        }
        setConnection({
          label: "Connection error",
          detail: connectionErrorText(error),
        });
      }
    }

    void loadConnection();

    return () => {
      cancelled = true;
    };
  }, []);

  function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setDraft("");
  }

  return (
    <main className="app-shell" aria-label="Codex mobile workbench">
      <ConnectionBar connection={connection} statusText={statusText} />

      <section className="workbench" aria-label="Workbench">
        <ApprovalQueue approvals={approvals} />

        <section className="session-grid" aria-label="Sessions and detail">
          <SessionList
            sessions={sessions}
            selectedThreadId={selectedSession.threadId}
            onSelect={setSelectedThreadId}
          />
          <SessionDetail
            approvals={selectedApprovals}
            events={selectedEvents}
            session={selectedSession}
          />
        </section>
      </section>

      <Composer
        draft={draft}
        selectedTitle={selectedSession.title}
        onDraftChange={setDraft}
        onSubmit={handleSubmit}
      />
    </main>
  );
}

function ConnectionBar({
  connection,
  statusText,
}: {
  connection: ConnectionViewState;
  statusText: string;
}) {
  return (
    <header className="connection-bar" aria-label="Connection status">
      <div className="connection-primary">
        <span className={`status-dot ${connectionClass(connection.label)}`} aria-hidden="true" />
        <div>
          <p className="eyebrow">LAN bridge</p>
          <h1>Codex Mobile</h1>
          {connection.detail ? <p className="connection-detail">{connection.detail}</p> : null}
        </div>
      </div>
      <div className="connection-meta">
        <span className={`meta-chip ${connectionClass(connection.label)}`}>{connection.label}</span>
        <span className="meta-chip muted">{statusText}</span>
      </div>
    </header>
  );
}

function ApprovalQueue({ approvals }: { approvals: ApprovalRequest[] }) {
  return (
    <section className="approval-queue" aria-labelledby="approval-heading">
      <div className="section-heading">
        <h2 id="approval-heading">Pending approvals</h2>
        <span>{approvals.length}</span>
      </div>
      <div className="approval-strip">
        {approvals.map((approval) => (
          <article className="approval-card" key={approval.id}>
            <div className="approval-icon" aria-hidden="true">
              <ApprovalIcon kind={approval.kind} />
            </div>
            <div className="approval-body">
              <div className="approval-title-row">
                <h3>{approval.title}</h3>
                <span>{approval.kind.replace("_", " ")}</span>
              </div>
              <p className="approval-detail">{approval.detail}</p>
              {approval.riskHint ? (
                <p className="risk-line">
                  <ShieldAlert size={13} aria-hidden="true" />
                  {approval.riskHint}
                </p>
              ) : null}
            </div>
            <div className="approval-actions" aria-label={`${approval.title} decision`}>
              <button className="icon-button danger" type="button" aria-label={`Reject ${approval.title}`}>
                <X size={16} />
              </button>
              <button className="icon-button success" type="button" aria-label={`Approve ${approval.title}`}>
                <Check size={16} />
              </button>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}

function SessionList({
  onSelect,
  selectedThreadId,
  sessions: sessionItems,
}: {
  onSelect: (threadId: string) => void;
  selectedThreadId: string;
  sessions: SessionSnapshot[];
}) {
  return (
    <section className="session-list-panel" aria-labelledby="session-list-heading">
      <div className="section-heading">
        <h2 id="session-list-heading">Sessions</h2>
        <span>{sessionItems.length}</span>
      </div>
      <div className="session-list" role="list">
        {sessionItems.map((session) => {
          const selected = session.threadId === selectedThreadId;
          return (
            <button
              aria-current={selected ? "true" : undefined}
              className="session-row"
              key={session.threadId}
              onClick={() => onSelect(session.threadId)}
              type="button"
            >
              <span className={`session-state ${session.status}`} aria-hidden="true">
                <CircleDot size={14} />
              </span>
              <span className="session-row-body">
                <span className="session-title-line">
                  <strong>{session.title}</strong>
                  {session.pendingApprovalIds.length > 0 ? (
                    <span className="pending-pill">{session.pendingApprovalIds.length}</span>
                  ) : null}
                </span>
                <span>{session.preview}</span>
              </span>
              <ChevronRight size={15} aria-hidden="true" />
            </button>
          );
        })}
      </div>
    </section>
  );
}

function SessionDetail({
  approvals: sessionApprovals,
  events: sessionEvents,
  session,
}: {
  approvals: ApprovalRequest[];
  events: SessionEvent[];
  session: SessionSnapshot;
}) {
  return (
    <section className="session-detail" aria-labelledby="session-detail-heading">
      <div className="detail-header">
        <div>
          <p className="eyebrow">Selected thread</p>
          <h2 id="session-detail-heading">{session.title}</h2>
        </div>
        <StatusBadge status={session.status} />
      </div>

      <dl className="session-facts">
        <div>
          <dt>Workspace</dt>
          <dd>{session.cwd ?? "Unknown"}</dd>
        </div>
        <div>
          <dt>Model</dt>
          <dd>{session.modelProvider ?? "Unset"}</dd>
        </div>
      </dl>

      {sessionApprovals.length > 0 ? (
        <div className="inline-alert" role="status">
          <AlertTriangle size={15} aria-hidden="true" />
          {sessionApprovals.length} approval blocks this thread.
        </div>
      ) : null}

      <div className="event-stream" aria-label="Session event stream">
        {sessionEvents.map((event) => (
          <article className="event-row" key={event.id}>
            <span className="event-icon" aria-hidden="true">
              {event.type === "tool_call" ? <TerminalSquare size={14} /> : <Clock3 size={14} />}
            </span>
            <div>
              <p>{event.type.replace("_", " ")}</p>
              <span>{payloadText(event.payload)}</span>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}

function Composer({
  draft,
  onDraftChange,
  onSubmit,
  selectedTitle,
}: {
  draft: string;
  onDraftChange: (value: string) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  selectedTitle: string;
}) {
  return (
    <form className="composer" aria-label="Message composer" onSubmit={onSubmit}>
      <label className="sr-only" htmlFor="codex-message">
        Message selected Codex session
      </label>
      <textarea
        id="codex-message"
        name="message"
        onChange={(event) => onDraftChange(event.target.value)}
        placeholder={`Message ${selectedTitle}`}
        rows={1}
        value={draft}
      />
      <button className="send-button" type="submit" aria-label="Send message" disabled={!draft.trim()}>
        <Send size={17} />
      </button>
    </form>
  );
}

function ApprovalIcon({ kind }: { kind: ApprovalKind }) {
  if (kind === "file_edit") {
    return <FilePenLine size={16} />;
  }
  return <Command size={16} />;
}

function StatusBadge({ status }: { status: SessionSnapshot["status"] }) {
  return <span className={`status-badge ${status}`}>{status.replaceAll("_", " ")}</span>;
}

function payloadText(payload: SessionEvent["payload"]) {
  if (payload && typeof payload === "object" && !Array.isArray(payload) && "text" in payload) {
    const value = payload.text;
    return typeof value === "string" ? value : JSON.stringify(value);
  }
  return JSON.stringify(payload);
}

function isExpired(expiresAt: number): boolean {
  return expiresAt <= Date.now();
}

function completePairingOnce(
  bridgeUrl: string,
  pairingPayload: PairingPayload,
  savedSession: DeviceSession | null,
): Promise<DeviceSession> {
  const attemptKey = `${bridgeUrl}:${pairingPayload.pairingToken}`;
  const existingAttempt = pairingAttempts.get(attemptKey);
  if (existingAttempt) {
    return existingAttempt;
  }

  const attempt = completePairingWithDevice(bridgeUrl, pairingPayload, savedSession).finally(() => {
    pairingAttempts.delete(attemptKey);
  });
  pairingAttempts.set(attemptKey, attempt);
  return attempt;
}

async function completePairingWithDevice(
  bridgeUrl: string,
  pairingPayload: PairingPayload,
  savedSession: DeviceSession | null,
): Promise<DeviceSession> {
  const device = createDeviceSession({
    bridgeUrl,
    displayName: pairingPayload.displayName,
    existing: savedSession,
  });
  const sessionResponse = await completePairing(bridgeUrl, {
    pairingToken: pairingPayload.pairingToken,
    deviceId: device.deviceId,
    displayName: device.displayName,
    deviceSecret: device.deviceSecret,
  });
  const pairedSession: DeviceSession = {
    ...device,
    deviceId: sessionResponse.deviceId,
    sessionToken: sessionResponse.sessionToken,
    sessionExpiresAt: sessionResponse.sessionExpiresAt,
    bridgeUrl,
  };
  saveSession(pairedSession);
  return pairedSession;
}

async function getHealthWithRefresh(bridgeUrl: string, session: DeviceSession): Promise<HealthResponse> {
  try {
    return await getHealth(bridgeUrl, session.sessionToken);
  } catch (error) {
    if (!isAuthError(error)) {
      throw error;
    }
  }

  const refreshed = await refreshSession(bridgeUrl, session);
  const nextSession = {
    ...session,
    deviceId: refreshed.deviceId,
    sessionToken: refreshed.sessionToken,
    sessionExpiresAt: refreshed.sessionExpiresAt,
    bridgeUrl,
  };
  saveSession(nextSession);
  return getHealth(bridgeUrl, nextSession.sessionToken);
}

function mapHealthToConnection(health: HealthResponse): ConnectionViewState {
  const state = health.connectionState.toLowerCase().replaceAll("-", "_");
  if (state === "codex_not_running" || state === "not_running") {
    return { label: "Codex not running" };
  }
  if (state === "inject_failed" || state === "injection_failed") {
    return { label: "Inject failed" };
  }
  if (state === "read_only" || state === "readonly") {
    return { label: "Read-only" };
  }
  if (state === "writable" || state === "ready") {
    return { label: "Writable" };
  }
  if (state === "connected" || health.status.toLowerCase() === "ok") {
    return { label: "Connected" };
  }
  return { label: "Connection error", detail: health.connectionState };
}

function connectionErrorText(error: unknown): string {
  if (isAuthError(error)) {
    return "Session revoked or expired";
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "Unable to reach bridge";
}

function isAuthError(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 401 || error.status === 403);
}

function clearPairingParamsFromUrl(): void {
  const url = new URL(window.location.href);
  let changed = false;
  for (const param of ["pairingToken", "token", "bridgeUrl", "displayName", "deviceName"]) {
    if (url.searchParams.has(param)) {
      url.searchParams.delete(param);
      changed = true;
    }
  }

  if (changed) {
    window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
  }
}

function connectionClass(label: ConnectionLabel): string {
  switch (label) {
    case "Connected":
    case "Writable":
      return "ok";
    case "Pairing":
    case "Read-only":
      return "warn";
    case "Unpaired":
      return "muted";
    default:
      return "error";
  }
}

export default App;
