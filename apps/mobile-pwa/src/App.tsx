import {
  FormEvent,
  useEffect,
  useLayoutEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type ReactNode,
  type SetStateAction,
} from "react";
import {
  AlertTriangle,
  Check,
  ChevronRight,
  CircleDot,
  Clock3,
  Command,
  FilePenLine,
  Menu,
  Send,
  ShieldAlert,
  TerminalSquare,
  X,
} from "lucide-react";
import type {
  ApprovalDecision,
  ApprovalKind,
  ApprovalRequest,
  DecisionKind,
  ImageAttachment,
  ServerEnvelope,
  SessionEvent,
  SessionSnapshot,
} from "./protocol";
import {
  ApiError,
  completePairing,
  connectWebSocket,
  decideApproval,
  fetchAssetBlob,
  getHealth,
  listSessionEvents,
  listSessions,
  readPairingPayloadFromUrl,
  refreshSession,
  sendTextMessage,
  type HealthResponse,
  type PairingPayload,
} from "./api";
import { createDeviceSession, loadSession, saveSession, type DeviceSession } from "./storage";

const sampleSessions: SessionSnapshot[] = [
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

const sampleApprovals: ApprovalRequest[] = [
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

const sampleEvents: SessionEvent[] = [
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
const SESSION_LIST_REFRESH_MS = 5_000;
const SESSION_EVENTS_REFRESH_MS = 2_000;

function App() {
  const hasInitialPairingPayload = useMemo(() => readPairingPayloadFromUrl(window.location.href) !== null, []);
  const [selectedThreadId, setSelectedThreadId] = useState(sampleSessions[0].threadId);
  const [draft, setDraft] = useState("");
  const [isSessionDrawerOpen, setIsSessionDrawerOpen] = useState(false);
  const [connection, setConnection] = useState<ConnectionViewState>({ label: "Unpaired" });
  const [deviceSession, setDeviceSession] = useState<DeviceSession | null>(null);
  const [liveSessions, setLiveSessions] = useState<SessionSnapshot[] | null>(null);
  const [eventsByThread, setEventsByThread] = useState<Record<string, SessionEvent[]>>({});
  const [liveApprovals, setLiveApprovals] = useState<ApprovalRequest[]>([]);
  const [sending, setSending] = useState(false);
  const [decidingApprovalIds, setDecidingApprovalIds] = useState<Record<string, DecisionKind>>({});
  const showSampleData = connection.label === "Unpaired" && !hasInitialPairingPayload && !deviceSession;
  const sessions = liveSessions ?? (showSampleData ? sampleSessions : []);
  const approvals = showSampleData ? sampleApprovals : liveApprovals;
  const selectedSession = sessions.find((session) => session.threadId === selectedThreadId) ?? null;
  const selectedApprovals = selectedSession
    ? approvals.filter((approval) => approval.threadId === selectedSession.threadId)
    : [];
  const selectedEvents =
    selectedSession
      ? eventsByThread[selectedSession.threadId] ??
        (showSampleData ? sampleEvents.filter((event) => event.threadId === selectedSession.threadId) : [])
      : [];
  const pendingCount = approvals.length;
  const canSend = (connection.label === "Connected" || connection.label === "Writable") && Boolean(deviceSession) && Boolean(selectedSession);

  const statusText = useMemo(() => {
    if (pendingCount > 0) {
      return `${pendingCount} pending`;
    }
    return secondaryStatusText(connection.label);
  }, [connection.label, pendingCount]);

  useEffect(() => {
    let cancelled = false;

    async function loadConnection() {
      const pairingPayload = readPairingPayloadFromUrl(window.location.href);
      const savedSession = loadSession();

      try {
        if (pairingPayload) {
          const pairingBridgeUrl = pairingPayload.bridgeUrl ?? window.location.origin;
          setConnection({ label: "Pairing" });
          try {
            const pairedSession = await completePairingOnce(pairingBridgeUrl, pairingPayload, savedSession);
            clearPairingParamsFromUrl();
            const health = await getHealth(pairingBridgeUrl, pairedSession.sessionToken);
            if (!cancelled) {
              setConnection(mapHealthToConnection(health));
              setDeviceSession(pairedSession);
            }
            return;
          } catch (error) {
            if (!savedSession || !isPairingTokenError(error)) {
              throw error;
            }
            clearPairingParamsFromUrl();
          }
        }

        if (!savedSession) {
          setConnection({ label: "Unpaired" });
          setDeviceSession(null);
          return;
        }

        const bridgeUrl = savedSession.bridgeUrl ?? window.location.origin;

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
            setDeviceSession(nextSession);
          }
          return;
        }

        setConnection({ label: "Connected" });
        const { health, session } = await getHealthWithRefresh(bridgeUrl, savedSession);
        if (!cancelled) {
          setConnection(mapHealthToConnection(health));
          setDeviceSession(session);
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

  useEffect(() => {
    if (!deviceSession || !isSessionDataEnabled(connection.label)) {
      return;
    }

    let cancelled = false;
    const activeSession = deviceSession;

    async function loadSessionList() {
      try {
        const items = await listSessions(activeSession.bridgeUrl, activeSession.sessionToken);
        if (cancelled) {
          return;
        }
        const sorted = sortSessions(items);
        setLiveSessions(sorted);
        setSelectedThreadId((current) => {
          if (sorted.some((session) => session.threadId === current)) {
            return current;
          }
          return sorted[0]?.threadId ?? "";
        });
      } catch (error) {
        if (!cancelled) {
          setConnection({
            label: "Connection error",
            detail: connectionErrorText(error),
          });
        }
      }
    }

    void loadSessionList();
    const intervalId = window.setInterval(() => {
      void loadSessionList();
    }, SESSION_LIST_REFRESH_MS);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, [connection.label, deviceSession]);

  useEffect(() => {
    if (!deviceSession || !liveSessions || !selectedSession) {
      return;
    }

    let cancelled = false;
    const activeSession = deviceSession;
    const threadId = selectedSession.threadId;

    async function loadEvents() {
      try {
        const items = await listSessionEvents(activeSession.bridgeUrl, activeSession.sessionToken, threadId);
        if (!cancelled) {
          setEventsByThread((current) => ({
            ...current,
            [threadId]: mergePolledSessionEvents(current[threadId] ?? [], items),
          }));
        }
      } catch (error) {
        if (!cancelled) {
          setConnection({
            label: "Connection error",
            detail: connectionErrorText(error),
          });
        }
      }
    }

    void loadEvents();
    const intervalId = window.setInterval(() => {
      void loadEvents();
    }, SESSION_EVENTS_REFRESH_MS);

    return () => {
      cancelled = true;
      window.clearInterval(intervalId);
    };
  }, [deviceSession, liveSessions, selectedSession]);

  useEffect(() => {
    if (!deviceSession || !isSessionDataEnabled(connection.label) || typeof WebSocket === "undefined") {
      return;
    }

    const ws = connectWebSocket(deviceSession.bridgeUrl, deviceSession.sessionToken);
    ws.onmessage = (message) => {
      const envelope = parseServerEnvelope(message.data);
      if (!envelope) {
        return;
      }
      handleServerEnvelope(envelope, setLiveSessions, setEventsByThread, setLiveApprovals);
    };

    return () => {
      ws.close();
    };
  }, [connection.label, deviceSession]);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const activeSession = deviceSession;
    if (!canSend || sending || !draft.trim() || !activeSession || !selectedSession) {
      return;
    }

    const text = draft.trim();
    const threadId = selectedSession.threadId;
    setSending(true);
    setEventsByThread((current) => {
      const localEvent: SessionEvent = {
        id: `local-${Date.now()}`,
        threadId,
        type: "message",
        payload: { role: "user", text, pending: true },
        createdAt: Date.now(),
      };
      return {
        ...current,
        [threadId]: sortSessionEvents(appendOrMergeSessionEvent(current[threadId] ?? [], localEvent)),
      };
    });
    setDraft("");
    try {
      await sendTextMessage(activeSession.bridgeUrl, activeSession.sessionToken, threadId, text);
    } catch (error) {
      setConnection({ label: "Connection error", detail: connectionErrorText(error) });
    } finally {
      setSending(false);
    }
  }

  async function handleApprovalDecision(approval: ApprovalRequest, decision: DecisionKind) {
    const activeSession = deviceSession;
    if (!activeSession || decidingApprovalIds[approval.id]) {
      return;
    }

    setDecidingApprovalIds((current) => ({ ...current, [approval.id]: decision }));
    try {
      await decideApproval(activeSession.bridgeUrl, activeSession.sessionToken, approval.id, decision);
      const decidedAt = Date.now();
      const resolved: ApprovalDecision = {
        approvalId: approval.id,
        decision,
        deviceId: activeSession.deviceId,
        decidedAt,
      };
      setLiveApprovals((current) => removeResolvedApproval(current, resolved));
      setEventsByThread((current) => ({
        ...current,
        [approval.threadId]: appendOrMergeSessionEvent(current[approval.threadId] ?? [], {
          id: `approval-${approval.id}-${decision}-${decidedAt}`,
          threadId: approval.threadId,
          type: "approval_resolved",
          payload: {
            approvalId: approval.id,
            decision,
            title: approval.title,
          },
          createdAt: decidedAt,
        }),
      }));
    } catch (error) {
      setConnection({ label: "Connection error", detail: connectionErrorText(error) });
    } finally {
      setDecidingApprovalIds((current) => {
        const next = { ...current };
        delete next[approval.id];
        return next;
      });
    }
  }

  function handleSelectSession(threadId: string) {
    setSelectedThreadId(threadId);
    setIsSessionDrawerOpen(false);
  }

  return (
    <main className="app-shell" aria-label="Codex mobile workbench">
      <ConnectionBar
        connection={connection}
        statusText={statusText}
        showSessionMenuButton
        onOpenSessions={() => setIsSessionDrawerOpen(true)}
      />

      <section className="workbench" aria-label="Workbench">
        <ApprovalQueue
          approvals={approvals}
          decidingApprovalIds={decidingApprovalIds}
          onDecision={handleApprovalDecision}
        />

        <section className="session-grid" aria-label="Sessions and detail">
          <div className="desktop-session-panel">
            <SessionList
              sessions={sessions}
              selectedThreadId={selectedSession?.threadId ?? ""}
              onSelect={setSelectedThreadId}
            />
          </div>
          <SessionDetail
            assetSession={deviceSession}
            approvals={selectedApprovals}
            events={selectedEvents}
            session={selectedSession}
          />
        </section>
      </section>

      <SessionDrawer
        isOpen={isSessionDrawerOpen}
        sessions={sessions}
        selectedThreadId={selectedSession?.threadId ?? ""}
        onClose={() => setIsSessionDrawerOpen(false)}
        onSelect={handleSelectSession}
      />

      <Composer
        draft={draft}
        selectedTitle={selectedSession?.title ?? "No session selected"}
        disabled={!canSend || sending}
        onDraftChange={setDraft}
        onSubmit={handleSubmit}
      />
    </main>
  );
}

function ConnectionBar({
  connection,
  onOpenSessions,
  showSessionMenuButton = false,
  statusText,
}: {
  connection: ConnectionViewState;
  onOpenSessions?: () => void;
  showSessionMenuButton?: boolean;
  statusText: string;
}) {
  return (
    <header className="connection-bar" aria-label="Connection status">
      {showSessionMenuButton ? (
        <button className="session-menu-button" onClick={onOpenSessions} type="button" aria-label="Open sessions">
          <Menu size={18} aria-hidden="true" />
        </button>
      ) : null}
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

function ApprovalQueue({
  approvals,
  decidingApprovalIds,
  onDecision,
}: {
  approvals: ApprovalRequest[];
  decidingApprovalIds: Record<string, DecisionKind>;
  onDecision: (approval: ApprovalRequest, decision: DecisionKind) => void;
}) {
  return (
    <section className="approval-queue" aria-labelledby="approval-heading">
      <div className="section-heading">
        <h2 id="approval-heading">Pending approvals</h2>
        <span>{approvals.length}</span>
      </div>
      <div className="approval-strip">
        {approvals.map((approval) => {
          const pendingDecision = decidingApprovalIds[approval.id];
          const disabled = Boolean(pendingDecision);
          return (
            <article className="approval-card" key={approval.id} aria-busy={disabled}>
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
                <button
                  className="icon-button danger"
                  disabled={disabled}
                  onClick={() => onDecision(approval, "reject")}
                  type="button"
                  aria-label={`Reject ${approval.title}`}
                >
                  <X size={16} />
                </button>
                <button
                  className="icon-button success"
                  disabled={disabled}
                  onClick={() => onDecision(approval, "approve")}
                  type="button"
                  aria-label={`Approve ${approval.title}`}
                >
                  <Check size={16} />
                </button>
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}

function SessionDrawer({
  isOpen,
  onClose,
  onSelect,
  selectedThreadId,
  sessions,
}: {
  isOpen: boolean;
  onClose: () => void;
  onSelect: (threadId: string) => void;
  selectedThreadId: string;
  sessions: SessionSnapshot[];
}) {
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    closeButtonRef.current?.focus();
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        onClose();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isOpen, onClose]);

  if (!isOpen) {
    return null;
  }

  return (
    <div className="session-drawer-layer">
      <button className="session-drawer-backdrop" onClick={onClose} type="button" aria-label="Close sessions drawer" />
      <aside className="session-drawer" role="dialog" aria-modal="true" aria-label="Sessions">
        <div className="drawer-heading">
          <h2>Sessions</h2>
          <div className="drawer-heading-actions">
            <span>{sessions.length}</span>
            <button ref={closeButtonRef} className="icon-button" onClick={onClose} type="button" aria-label="Close sessions">
              <X size={16} aria-hidden="true" />
            </button>
          </div>
        </div>
        <SessionList sessions={sessions} selectedThreadId={selectedThreadId} onSelect={onSelect} />
      </aside>
    </div>
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
  const headingId = useId();

  return (
    <section className="session-list-panel" aria-labelledby={headingId}>
      <div className="section-heading">
        <h2 id={headingId}>Sessions</h2>
        <span>{sessionItems.length}</span>
      </div>
      <div className="session-list" role="list">
        {sessionItems.length === 0 ? (
          <div className="empty-state" role="status">
            No live sessions yet. Use the newest pairing URL from the bridge terminal.
          </div>
        ) : null}
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
  assetSession,
  approvals: sessionApprovals,
  events: sessionEvents,
  session,
}: {
  assetSession: DeviceSession | null;
  approvals: ApprovalRequest[];
  events: SessionEvent[];
  session: SessionSnapshot | null;
}) {
  const eventStreamRef = useRef<HTMLDivElement | null>(null);
  const shouldStickToBottomRef = useRef(true);
  const previousThreadIdRef = useRef<string | null>(null);
  const threadId = session ? session.threadId : "";
  const eventTail = sessionEvents.at(-1);
  const eventTailKey = eventTail
    ? `${eventTail.id}:${eventTail.createdAt}:${payloadText(eventTail.payload).length}`
    : "";

  useLayoutEffect(() => {
    if (!threadId) {
      return;
    }
    const stream = eventStreamRef.current;
    if (!stream) {
      return;
    }

    const threadChanged = previousThreadIdRef.current !== threadId;
    if (threadChanged || shouldStickToBottomRef.current) {
      if (typeof stream.scrollTo === "function") {
        stream.scrollTo({ top: stream.scrollHeight, behavior: "auto" });
      } else {
        stream.scrollTop = stream.scrollHeight;
      }
      shouldStickToBottomRef.current = true;
    }
    previousThreadIdRef.current = threadId;
  }, [eventTailKey, sessionEvents.length, threadId]);

  function handleEventStreamScroll() {
    const stream = eventStreamRef.current;
    if (!stream) {
      return;
    }
    shouldStickToBottomRef.current =
      stream.scrollHeight - stream.scrollTop - stream.clientHeight < 80;
  }

  if (!session) {
    return (
      <section className="session-detail empty-session-detail" aria-labelledby="session-detail-heading">
        <div className="detail-header">
          <div>
            <p className="eyebrow">Selected thread</p>
            <h2 id="session-detail-heading">No sessions available</h2>
          </div>
        </div>
        <div className="empty-state" role="status">
          Pair with an active Codex bridge session to view thread events.
        </div>
      </section>
    );
  }

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

      <div
        className="event-stream"
        aria-label="Session event stream"
        ref={eventStreamRef}
        onScroll={handleEventStreamScroll}
      >
        {sessionEvents.map((event) => (
          <EventRow assetSession={assetSession} event={event} key={event.id} />
        ))}
      </div>
    </section>
  );
}

function EventRow({
  assetSession,
  event,
}: {
  assetSession: DeviceSession | null;
  event: SessionEvent;
}) {
  const attachments = payloadImageAttachments(event.payload);

  return (
    <article className="event-row">
      <span className="event-icon" aria-hidden="true">
        {event.type === "tool_call" ? <TerminalSquare size={14} /> : <Clock3 size={14} />}
      </span>
      <div className="event-content">
        <p className="event-kind">{event.type.replace("_", " ")}</p>
        <MessageBody text={payloadText(event.payload)} />
        {attachments.length > 0 ? (
          <div className="attachment-list" aria-label="Image attachments">
            {attachments.map((attachment, index) => (
              <AttachmentImage
                assetSession={assetSession}
                attachment={attachment}
                key={`${attachment.src}:${index}`}
              />
            ))}
          </div>
        ) : null}
      </div>
    </article>
  );
}

function AttachmentImage({
  assetSession,
  attachment,
}: {
  assetSession: DeviceSession | null;
  attachment: ImageAttachment;
}) {
  const [objectUrl, setObjectUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const bridgeUrl = assetSession?.bridgeUrl ?? "";
  const sessionToken = assetSession?.sessionToken ?? "";

  useEffect(() => {
    let cancelled = false;
    let createdObjectUrl: string | null = null;

    setObjectUrl(null);
    setFailed(false);

    if (!bridgeUrl || !sessionToken) {
      setFailed(true);
      return;
    }

    async function loadAttachment() {
      try {
        const blob = await fetchAssetBlob(bridgeUrl, sessionToken, attachment.src);
        if (cancelled) {
          return;
        }
        createdObjectUrl = URL.createObjectURL(blob);
        setObjectUrl(createdObjectUrl);
      } catch {
        if (!cancelled) {
          setFailed(true);
        }
      }
    }

    void loadAttachment();

    return () => {
      cancelled = true;
      if (createdObjectUrl) {
        URL.revokeObjectURL(createdObjectUrl);
      }
    };
  }, [bridgeUrl, sessionToken, attachment.src]);

  if (failed) {
    return <span className="attachment-error" role="status">Image unavailable: {attachment.name}</span>;
  }

  if (!objectUrl) {
    return <span className="attachment-loading" role="status">Loading image: {attachment.name}</span>;
  }

  return <img className="attachment-image" src={objectUrl} alt={attachment.name} />;
}

function Composer({
  disabled,
  draft,
  onDraftChange,
  onSubmit,
  selectedTitle,
}: {
  disabled: boolean;
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
        disabled={disabled}
        value={draft}
      />
      <button className="send-button" type="submit" aria-label="Send message" disabled={disabled || !draft.trim()}>
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

type MarkdownBlock =
  | { type: "paragraph"; text: string }
  | { type: "heading"; level: 1 | 2 | 3; text: string }
  | { type: "list"; ordered: boolean; items: string[] }
  | { type: "code"; language?: string; text: string }
  | { type: "quote"; text: string };

function MessageBody({ text }: { text: string }) {
  const blocks = parseMessageMarkdown(text);

  return (
    <div className="message-body">
      {blocks.map((block, index) => renderMarkdownBlock(block, index))}
    </div>
  );
}

function renderMarkdownBlock(block: MarkdownBlock, index: number) {
  if (block.type === "heading") {
    const HeadingTag = `h${block.level}` as "h1" | "h2" | "h3";
    return <HeadingTag key={index}>{renderInlineMarkdown(block.text)}</HeadingTag>;
  }

  if (block.type === "list") {
    const ListTag = block.ordered ? "ol" : "ul";
    return (
      <ListTag key={index}>
        {block.items.map((item, itemIndex) => (
          <li key={itemIndex}>{renderInlineMarkdown(item)}</li>
        ))}
      </ListTag>
    );
  }

  if (block.type === "code") {
    return (
      <pre key={index} data-language={block.language || undefined}>
        <code>{block.text}</code>
      </pre>
    );
  }

  if (block.type === "quote") {
    return <blockquote key={index}>{renderInlineMarkdown(block.text)}</blockquote>;
  }

  return <p key={index}>{renderInlineMarkdown(block.text)}</p>;
}

function parseMessageMarkdown(text: string): MarkdownBlock[] {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const blocks: MarkdownBlock[] = [];
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    const trimmed = line.trim();

    if (!trimmed) {
      index += 1;
      continue;
    }

    const fenceMatch = trimmed.match(/^```(\S*)\s*$/);
    if (fenceMatch) {
      const codeLines: string[] = [];
      index += 1;
      while (index < lines.length && !lines[index].trim().startsWith("```")) {
        codeLines.push(lines[index]);
        index += 1;
      }
      if (index < lines.length) {
        index += 1;
      }
      blocks.push({ type: "code", language: fenceMatch[1] || undefined, text: codeLines.join("\n") });
      continue;
    }

    const headingMatch = trimmed.match(/^(#{1,3})\s+(.+)$/);
    if (headingMatch) {
      blocks.push({
        type: "heading",
        level: headingMatch[1].length as 1 | 2 | 3,
        text: headingMatch[2],
      });
      index += 1;
      continue;
    }

    const unorderedMatch = trimmed.match(/^[-*•]\s+(.+)$/);
    if (unorderedMatch) {
      const items: string[] = [];
      while (index < lines.length) {
        const itemMatch = lines[index].trim().match(/^[-*•]\s+(.+)$/);
        if (!itemMatch) {
          break;
        }
        items.push(itemMatch[1]);
        index += 1;
      }
      blocks.push({ type: "list", ordered: false, items });
      continue;
    }

    const orderedMatch = trimmed.match(/^\d+[.)]\s+(.+)$/);
    if (orderedMatch) {
      const items: string[] = [];
      while (index < lines.length) {
        const itemMatch = lines[index].trim().match(/^\d+[.)]\s+(.+)$/);
        if (!itemMatch) {
          break;
        }
        items.push(itemMatch[1]);
        index += 1;
      }
      blocks.push({ type: "list", ordered: true, items });
      continue;
    }

    const quoteMatch = trimmed.match(/^>\s?(.*)$/);
    if (quoteMatch) {
      const quoteLines: string[] = [];
      while (index < lines.length) {
        const itemMatch = lines[index].trim().match(/^>\s?(.*)$/);
        if (!itemMatch) {
          break;
        }
        quoteLines.push(itemMatch[1]);
        index += 1;
      }
      blocks.push({ type: "quote", text: quoteLines.join("\n") });
      continue;
    }

    const paragraphLines = [trimmed];
    index += 1;
    while (index < lines.length) {
      const next = lines[index].trim();
      if (!next || isMarkdownBlockStart(next)) {
        break;
      }
      paragraphLines.push(next);
      index += 1;
    }
    blocks.push({ type: "paragraph", text: paragraphLines.join("\n") });
  }

  return blocks.length > 0 ? blocks : [{ type: "paragraph", text }];
}

function isMarkdownBlockStart(text: string) {
  return (
    text.startsWith("```") ||
    /^#{1,3}\s+/.test(text) ||
    /^[-*•]\s+/.test(text) ||
    /^\d+[.)]\s+/.test(text) ||
    /^>\s?/.test(text)
  );
}

function renderInlineMarkdown(text: string) {
  const nodes: ReactNode[] = [];
  const pattern = /(`[^`]+`|\[([^\]]+)\]\(([^)\s]+)\))/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;

  while ((match = pattern.exec(text)) !== null) {
    pushTextWithBreaks(nodes, text.slice(lastIndex, match.index));
    const token = match[0];
    if (token.startsWith("`")) {
      nodes.push(<code key={nodes.length}>{token.slice(1, -1)}</code>);
    } else {
      const label = match[2];
      const href = safeMarkdownHref(match[3]);
      if (href) {
        nodes.push(
          <a href={href} key={nodes.length} rel="noreferrer" target="_blank">
            {label}
          </a>,
        );
      } else {
        nodes.push(token);
      }
    }
    lastIndex = match.index + token.length;
  }

  pushTextWithBreaks(nodes, text.slice(lastIndex));
  return nodes;
}

function pushTextWithBreaks(nodes: ReactNode[], value: string) {
  const parts = value.split("\n");
  parts.forEach((part, index) => {
    if (part) {
      nodes.push(part);
    }
    if (index < parts.length - 1) {
      nodes.push(<br key={`br-${nodes.length}`} />);
    }
  });
}

function safeMarkdownHref(href: string) {
  if (/^(https?:\/\/|mailto:)/i.test(href)) {
    return href;
  }
  return null;
}

function payloadText(payload: SessionEvent["payload"]) {
  if (payload && typeof payload === "object" && !Array.isArray(payload) && "text" in payload) {
    const value = payload.text;
    return typeof value === "string" ? value : JSON.stringify(value);
  }
  return JSON.stringify(payload);
}

function payloadImageAttachments(payload: SessionEvent["payload"]): ImageAttachment[] {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return [];
  }

  const attachments = payload.attachments;
  if (!Array.isArray(attachments)) {
    return [];
  }

  const images: ImageAttachment[] = [];
  for (const attachment of attachments as unknown[]) {
    if (isImageAttachment(attachment)) {
      images.push(attachment);
    }
  }
  return images;
}

function isImageAttachment(value: unknown): value is ImageAttachment {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const attachment = value as Record<string, unknown>;
  return (
    attachment.type === "image" &&
    typeof attachment.src === "string" &&
    typeof attachment.name === "string"
  );
}

export function mergeSessionEvents(events: SessionEvent[]): SessionEvent[] {
  return events.reduce<SessionEvent[]>((merged, event) => appendOrMergeSessionEvent(merged, event), []);
}

export function appendOrMergeSessionEvent(events: SessionEvent[], event: SessionEvent): SessionEvent[] {
  const existingIndex = events.findIndex((existing) => existing.id === event.id);
  if (existingIndex !== -1) {
    if (isSameSessionEvent(events[existingIndex], event)) {
      return events;
    }
    const nextEvents = [...events];
    nextEvents[existingIndex] = event;
    return nextEvents;
  }

  if (isUserMessage(event)) {
    const pendingIndex = events.findIndex((existing) => isMatchingPendingUserMessage(existing, event));
    if (pendingIndex !== -1) {
      const nextEvents = [...events];
      nextEvents[pendingIndex] = event;
      return nextEvents;
    }
  }

  if (event.type !== "message_delta") {
    return [...events, event];
  }

  const delta = deltaText(event.payload);
  if (!delta) {
    return [...events, event];
  }

  const candidate = events.at(-1);
  if (
    candidate &&
    candidate.threadId === event.threadId &&
    (candidate.type === "message_delta" || isAssistantMessage(candidate))
  ) {
    const nextEvents = [...events];
    nextEvents[nextEvents.length - 1] = {
      ...candidate,
      payload: {
        role: "assistant",
        text: `${payloadText(candidate.payload)}${delta}`,
      },
      createdAt: event.createdAt,
    };
    return nextEvents;
  }

  return [
    ...events,
    {
      ...event,
      payload: { role: "assistant", text: delta },
    },
  ];
}

function isSameSessionEvent(left: SessionEvent, right: SessionEvent): boolean {
  return (
    left.id === right.id &&
    left.threadId === right.threadId &&
    left.type === right.type &&
    left.createdAt === right.createdAt &&
    isSameJsonValue(left.payload, right.payload)
  );
}

function isSameJsonValue(left: SessionEvent["payload"], right: SessionEvent["payload"]): boolean {
  if (Object.is(left, right)) {
    return true;
  }
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right) || left.length !== right.length) {
      return false;
    }
    return left.every((item, index) => isSameJsonValue(item, right[index]));
  }
  if (
    left === null ||
    right === null ||
    typeof left !== "object" ||
    typeof right !== "object"
  ) {
    return false;
  }

  const leftKeys = Object.keys(left);
  const rightKeys = Object.keys(right);
  if (leftKeys.length !== rightKeys.length) {
    return false;
  }

  const leftRecord = left as Record<string, SessionEvent["payload"]>;
  const rightRecord = right as Record<string, SessionEvent["payload"]>;
  return leftKeys.every(
    (key) =>
      Object.prototype.hasOwnProperty.call(rightRecord, key) &&
      isSameJsonValue(leftRecord[key], rightRecord[key]),
  );
}

export function mergePolledSessionEvents(current: SessionEvent[], polled: SessionEvent[]): SessionEvent[] {
  const polledUserTexts = new Set(
    polled
      .map(normalizedUserMessageText)
      .filter((text): text is string => Boolean(text)),
  );
  const carried = current.filter((event) => {
    if (!isPendingPayload(event.payload)) {
      return false;
    }
    const text = normalizedUserMessageText(event);
    if (text && polledUserTexts.has(text)) {
      return false;
    }
    return true;
  });

  return sortSessionEvents(mergeSessionEvents([...polled, ...carried]));
}

function sortSessionEvents(events: SessionEvent[]): SessionEvent[] {
  return [...events].sort(compareSessionEvents);
}

function compareSessionEvents(left: SessionEvent, right: SessionEvent): number {
  const byCreatedAt = left.createdAt - right.createdAt;
  if (byCreatedAt !== 0) {
    return byCreatedAt;
  }

  const leftItem = eventItemOrder(left.id);
  const rightItem = eventItemOrder(right.id);
  if (leftItem && rightItem && leftItem.scope === rightItem.scope && leftItem.index !== rightItem.index) {
    return leftItem.index - rightItem.index;
  }

  return 0;
}

function eventItemOrder(id: string): { scope: string; index: number } | null {
  const match = /^(.*):item-(\d+)(?::.*)?$/.exec(id);
  if (!match) {
    return null;
  }

  return {
    scope: match[1],
    index: Number(match[2]),
  };
}

function deltaText(payload: SessionEvent["payload"]): string {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return "";
  }
  if (typeof payload.text === "string") {
    return payload.text;
  }
  if (typeof payload.delta === "string") {
    return payload.delta;
  }
  return "";
}

function isAssistantMessage(event: SessionEvent): boolean {
  const payload = event.payload;
  return (
    event.type === "message" &&
    payload !== null &&
    typeof payload === "object" &&
    !Array.isArray(payload) &&
    payload.role === "assistant"
  );
}

function isUserMessage(event: SessionEvent): boolean {
  const payload = event.payload;
  return (
    event.type === "message" &&
    payload !== null &&
    typeof payload === "object" &&
    !Array.isArray(payload) &&
    payload.role === "user" &&
    typeof payload.text === "string"
  );
}

function isMatchingPendingUserMessage(existing: SessionEvent, next: SessionEvent): boolean {
  const nextText = normalizedUserMessageText(next);
  if (existing.threadId !== next.threadId || nextText === null) {
    return false;
  }
  if (!isPendingPayload(existing.payload) || isPendingPayload(next.payload)) {
    return false;
  }
  return existing.payload.text.trim() === nextText;
}

function userMessageText(event: SessionEvent): string | null {
  if (!isUserMessage(event)) {
    return null;
  }
  const payload = event.payload;
  if (payload === null || typeof payload !== "object" || Array.isArray(payload)) {
    return null;
  }
  return typeof payload.text === "string" ? payload.text : null;
}

function normalizedUserMessageText(event: SessionEvent): string | null {
  return userMessageText(event)?.trim() ?? null;
}

function isPendingPayload(payload: SessionEvent["payload"]): payload is { role: "user"; text: string; pending: true } {
  return (
    payload !== null &&
    typeof payload === "object" &&
    !Array.isArray(payload) &&
    payload.role === "user" &&
    typeof payload.text === "string" &&
    payload.pending === true
  );
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

async function getHealthWithRefresh(
  bridgeUrl: string,
  session: DeviceSession,
): Promise<{ health: HealthResponse; session: DeviceSession }> {
  try {
    return { health: await getHealth(bridgeUrl, session.sessionToken), session };
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
  return { health: await getHealth(bridgeUrl, nextSession.sessionToken), session: nextSession };
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
  if (error instanceof ApiError && error.status === 400) {
    return "Pairing link expired or already used. Restart the bridge and open the newest pairing URL.";
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "Unable to reach bridge";
}

function secondaryStatusText(label: ConnectionLabel): string {
  switch (label) {
    case "Connected":
    case "Writable":
      return "Writable";
    case "Read-only":
      return "Read-only";
    case "Inject failed":
      return "Desktop bridge unavailable";
    case "Codex not running":
      return "Start Codex Desktop";
    case "Connection error":
      return "Needs new link";
    case "Pairing":
      return "Pairing";
    case "Unpaired":
      return "Open pairing link";
  }
}

function isAuthError(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 401 || error.status === 403);
}

function isPairingTokenError(error: unknown): boolean {
  return error instanceof ApiError && error.status === 400;
}

function isSessionDataEnabled(label: ConnectionLabel): boolean {
  return label === "Connected" || label === "Writable" || label === "Read-only";
}

function sortSessions(items: SessionSnapshot[]): SessionSnapshot[] {
  return [...items].sort((left, right) => right.updatedAt - left.updatedAt);
}

function parseServerEnvelope(data: unknown): ServerEnvelope | null {
  if (typeof data !== "string") {
    return null;
  }
  try {
    const value = JSON.parse(data) as unknown;
    return isServerEnvelope(value) ? value : null;
  } catch {
    return null;
  }
}

function isServerEnvelope(value: unknown): value is ServerEnvelope {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const envelope = value as Record<string, unknown>;
  switch (envelope.type) {
    case "session_snapshot":
      return isSessionSnapshot(envelope.payload);
    case "session_event":
      return isSessionEvent(envelope.payload);
    case "approval_request":
      return isApprovalRequest(envelope.payload);
    case "approval_resolved":
      return isApprovalDecision(envelope.payload);
    case "error":
      return isErrorPayload(envelope.payload);
    default:
      return false;
  }
}

function isSessionSnapshot(value: unknown): value is SessionSnapshot {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const session = value as Record<string, unknown>;
  return (
    typeof session.threadId === "string" &&
    typeof session.title === "string" &&
    (session.cwd === undefined || typeof session.cwd === "string") &&
    (session.modelProvider === undefined || typeof session.modelProvider === "string") &&
    (session.preview === undefined || typeof session.preview === "string") &&
    typeof session.updatedAt === "number" &&
    Number.isFinite(session.updatedAt) &&
    isSessionStatus(session.status) &&
    Array.isArray(session.pendingApprovalIds) &&
    session.pendingApprovalIds.every((id) => typeof id === "string")
  );
}

function isSessionEvent(value: unknown): value is SessionEvent {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const event = value as Record<string, unknown>;
  return (
    typeof event.id === "string" &&
    typeof event.threadId === "string" &&
    isSessionEventType(event.type) &&
    isJsonValue(event.payload) &&
    typeof event.createdAt === "number" &&
    Number.isFinite(event.createdAt)
  );
}

function isApprovalRequest(value: unknown): value is ApprovalRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const approval = value as Record<string, unknown>;
  return (
    typeof approval.id === "string" &&
    typeof approval.threadId === "string" &&
    isApprovalKind(approval.kind) &&
    typeof approval.title === "string" &&
    typeof approval.detail === "string" &&
    (approval.riskHint === undefined || typeof approval.riskHint === "string") &&
    (approval.raw === undefined || isJsonValue(approval.raw)) &&
    typeof approval.createdAt === "number" &&
    Number.isFinite(approval.createdAt) &&
    (approval.expiresAt === undefined ||
      (typeof approval.expiresAt === "number" && Number.isFinite(approval.expiresAt)))
  );
}

function isApprovalDecision(value: unknown): value is ApprovalDecision {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }

  const decision = value as Record<string, unknown>;
  return (
    typeof decision.approvalId === "string" &&
    (decision.decision === "approve" || decision.decision === "reject") &&
    (decision.comment === undefined || typeof decision.comment === "string") &&
    typeof decision.deviceId === "string" &&
    typeof decision.decidedAt === "number" &&
    Number.isFinite(decision.decidedAt)
  );
}

function isErrorPayload(value: unknown): value is { message: string } {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    typeof (value as Record<string, unknown>).message === "string"
  );
}

function isSessionStatus(value: unknown): value is SessionSnapshot["status"] {
  return (
    value === "idle" ||
    value === "running" ||
    value === "waiting_for_input" ||
    value === "waiting_for_approval" ||
    value === "error"
  );
}

function isSessionEventType(value: unknown): value is SessionEvent["type"] {
  return (
    value === "message" ||
    value === "message_delta" ||
    value === "tool_call" ||
    value === "tool_result" ||
    value === "approval_requested" ||
    value === "approval_resolved" ||
    value === "status_changed" ||
    value === "error"
  );
}

function isApprovalKind(value: unknown): value is ApprovalKind {
  return value === "command" || value === "file_edit" || value === "network" || value === "mcp" || value === "unknown";
}

function isJsonValue(value: unknown): value is SessionEvent["payload"] {
  if (
    value === null ||
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  ) {
    return true;
  }
  if (Array.isArray(value)) {
    return value.every(isJsonValue);
  }
  if (typeof value === "object") {
    return Object.values(value).every(isJsonValue);
  }
  return false;
}

function handleServerEnvelope(
  envelope: ServerEnvelope,
  setLiveSessions: Dispatch<SetStateAction<SessionSnapshot[] | null>>,
  setEventsByThread: Dispatch<SetStateAction<Record<string, SessionEvent[]>>>,
  setApprovals: Dispatch<SetStateAction<ApprovalRequest[]>>,
): void {
  switch (envelope.type) {
    case "session_snapshot":
      setLiveSessions((current) => sortSessions(upsertSession(current ?? [], envelope.payload)));
      break;
    case "session_event":
      setEventsByThread((current) => ({
        ...current,
        [envelope.payload.threadId]: sortSessionEvents(
          appendOrMergeSessionEvent(current[envelope.payload.threadId] ?? [], envelope.payload),
        ),
      }));
      break;
    case "approval_request":
      setApprovals((current) => upsertApproval(current, envelope.payload));
      break;
    case "approval_resolved":
      setApprovals((current) => removeResolvedApproval(current, envelope.payload));
      break;
    case "error":
      break;
  }
}

function upsertSession(items: SessionSnapshot[], next: SessionSnapshot): SessionSnapshot[] {
  const index = items.findIndex((item) => item.threadId === next.threadId);
  if (index === -1) {
    return [...items, next];
  }
  const updated = [...items];
  updated[index] = next;
  return updated;
}

function upsertApproval(items: ApprovalRequest[], next: ApprovalRequest): ApprovalRequest[] {
  const index = items.findIndex((item) => item.id === next.id);
  if (index === -1) {
    return [...items, next];
  }
  const updated = [...items];
  updated[index] = next;
  return updated;
}

function removeResolvedApproval(items: ApprovalRequest[], decision: ApprovalDecision): ApprovalRequest[] {
  return items.filter((item) => item.id !== decision.approvalId);
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
