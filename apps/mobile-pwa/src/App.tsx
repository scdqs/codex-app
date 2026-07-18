import {
  FormEvent,
  useEffect,
  useLayoutEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type Ref,
  type ReactNode,
  type SetStateAction,
} from "react";
import {
  AlertTriangle,
  Bot,
  Check,
  ChevronRight,
  CircleDot,
  Clock3,
  Command,
  FilePenLine,
  ImagePlus,
  Menu,
  Plus,
  RefreshCw,
  Send,
  ShieldAlert,
  TerminalSquare,
  UserRound,
  X,
} from "lucide-react";
import {
  isSessionDataEnabled,
  mapHealthToConnection,
  parseServerEnvelope,
  secondaryStatusText,
  type ConnectionLabel,
  type ConnectionViewState,
  type ApprovalDecision,
  type ApprovalKind,
  type ApprovalRequest,
  type DecisionKind,
  type ImageAttachment,
  type ServerEnvelope,
  type SessionEvent,
  type SessionSnapshot,
  type WorkspaceOption,
} from "@codex/bridge-protocol";
import {
  ApiError,
  completePairing,
  connectWebSocket,
  createSession,
  decideApproval,
  fetchAssetBlob,
  getHealth,
  listApprovals,
  listSessionEvents,
  listSessions,
  listWorkspaces,
  readPairingPayloadFromUrl,
  refreshSession,
  sendTextMessage,
  type HealthResponse,
  type OutgoingImageAttachment,
  type PairingPayload,
  type SessionEventPage,
  type SessionEventPageOptions,
} from "./api";
import { createDeviceSession, loadSession, saveSession, type DeviceSession } from "./storage";

const pairingAttempts = new Map<string, Promise<DeviceSession>>();
const SESSION_LIST_REFRESH_MS = 5_000;
const SESSION_EVENTS_REFRESH_MS = 2_000;
const INITIAL_EVENT_PAGE_LIMIT = 50;
const INCREMENTAL_EVENT_PAGE_LIMIT = 100;
const HIDDEN_PAGE_POLL_MULTIPLIER = 6;
const MAX_POLL_BACKOFF_MS = 30_000;
const TRANSIENT_FAILURES_BEFORE_NEW_LINK = 3;
const MAX_DRAFT_IMAGE_ATTACHMENTS = 4;
const MAX_DRAFT_IMAGE_BYTES = 8 * 1024 * 1024;
const SUPPORTED_DRAFT_IMAGE_TYPES = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);

interface DraftImageAttachment {
  id: string;
  file: File;
  name: string;
  mimeType: string;
  previewUrl: string;
  size: number;
}

interface SessionEventSyncState {
  initialized: boolean;
  beforeCursor?: string;
  afterCursor?: string;
  hasMoreBefore: boolean;
  loadingOlder: boolean;
}

interface RefreshedDeviceSession {
  session: DeviceSession;
  health: HealthResponse;
}

function App() {
  const [selectedThreadId, setSelectedThreadId] = useState("");
  const [draft, setDraft] = useState("");
  const [draftImages, setDraftImages] = useState<DraftImageAttachment[]>([]);
  const [draftAttachmentError, setDraftAttachmentError] = useState("");
  const [messageSendError, setMessageSendError] = useState("");
  const [isSessionDrawerOpen, setIsSessionDrawerOpen] = useState(false);
  const [isNewSessionSheetOpen, setIsNewSessionSheetOpen] = useState(false);
  const [newSessionDraft, setNewSessionDraft] = useState("");
  const [newSessionImages, setNewSessionImages] = useState<DraftImageAttachment[]>([]);
  const [newSessionAttachmentError, setNewSessionAttachmentError] = useState("");
  const [newSessionError, setNewSessionError] = useState("");
  const [newSessionWorkspaces, setNewSessionWorkspaces] = useState<WorkspaceOption[] | null>(null);
  const [newSessionWorkspaceCwd, setNewSessionWorkspaceCwd] = useState("");
  const [newSessionWorkspaceError, setNewSessionWorkspaceError] = useState("");
  const [newSessionWorkspacesLoading, setNewSessionWorkspacesLoading] = useState(false);
  const [connection, setConnection] = useState<ConnectionViewState>({ label: "Unpaired" });
  const [bridgeVersion, setBridgeVersion] = useState<string | null>(null);
  const [deviceSession, setDeviceSession] = useState<DeviceSession | null>(null);
  const [liveSessions, setLiveSessions] = useState<SessionSnapshot[] | null>(null);
  const [eventsByThread, setEventsByThread] = useState<Record<string, SessionEvent[]>>({});
  const [eventSyncByThread, setEventSyncByThread] = useState<Record<string, SessionEventSyncState>>({});
  const [liveApprovals, setLiveApprovals] = useState<ApprovalRequest[]>([]);
  const [socketReconnectNonce, setSocketReconnectNonce] = useState(0);
  const [sending, setSending] = useState(false);
  const [decidingApprovalIds, setDecidingApprovalIds] = useState<Record<string, DecisionKind>>({});
  const sessionMenuButtonRef = useRef<HTMLButtonElement | null>(null);
  const sessionRefreshPromiseRef = useRef<Promise<RefreshedDeviceSession> | null>(null);
  const workspaceLoadRequestRef = useRef(0);
  const createSessionRequestRef = useRef(false);
  const sessionListFailureCountRef = useRef(0);
  const sessionEventsFailureCountRef = useRef(0);
  const eventSyncByThreadRef = useRef<Record<string, SessionEventSyncState>>({});
  const draftImagesRef = useRef<DraftImageAttachment[]>([]);
  const newSessionImagesRef = useRef<DraftImageAttachment[]>([]);
  const canSyncSessionData =
    Boolean(deviceSession) &&
    (isSessionDataEnabled(connection.label) ||
      connection.label === "Connection error" ||
      connection.label === "Pairing");
  const sessionDataLoaded = liveSessions !== null;
  const sessions = liveSessions ?? [];
  const approvals = liveApprovals;
  const selectedSession = sessions.find((session) => session.threadId === selectedThreadId) ?? null;
  const selectedApprovals = selectedSession
    ? approvals.filter((approval) => approval.threadId === selectedSession.threadId)
    : [];
  const selectedEvents = selectedSession ? eventsByThread[selectedSession.threadId] ?? [] : [];
  const selectedEventSync = selectedSession ? eventSyncByThread[selectedSession.threadId] : undefined;
  const pendingCount = approvals.length;
  const canSend = (connection.label === "Connected" || connection.label === "Writable") && Boolean(deviceSession) && Boolean(selectedSession);
  const canCreateSession =
    (connection.label === "Connected" || connection.label === "Writable") &&
    Boolean(deviceSession) &&
    sessionDataLoaded;

  const statusText = useMemo(() => {
    if (pendingCount > 0) {
      return `${pendingCount} pending`;
    }
    return secondaryStatusText(connection.label);
  }, [connection.label, pendingCount]);

  function applyHealth(health: HealthResponse) {
    setBridgeVersion(health.version ?? null);
    setConnection(mapHealthToConnection(health));
  }

  function clearAuthenticatedSessionData() {
    setDeviceSession(null);
    setLiveSessions(null);
    setEventsByThread({});
    eventSyncByThreadRef.current = {};
    setEventSyncByThread({});
    setLiveApprovals([]);
    setSelectedThreadId("");
  }

  async function refreshActiveSession(activeSession: DeviceSession): Promise<RefreshedDeviceSession> {
    if (!sessionRefreshPromiseRef.current) {
      setConnection({ label: "Pairing", detail: "Refreshing session" });
      sessionRefreshPromiseRef.current = (async () => {
        const refreshed = await refreshSession(activeSession.bridgeUrl, activeSession);
        const nextSession: DeviceSession = {
          ...activeSession,
          deviceId: refreshed.deviceId,
          sessionToken: refreshed.sessionToken,
          sessionExpiresAt: refreshed.sessionExpiresAt,
          bridgeUrl: activeSession.bridgeUrl,
        };
        saveSession(nextSession);
        const health = await getHealth(activeSession.bridgeUrl, nextSession.sessionToken);
        setDeviceSession((current) => {
          if (!current || current.deviceId !== activeSession.deviceId) {
            return current;
          }
          return nextSession;
        });
        applyHealth(health);
        return { session: nextSession, health };
      })()
        .catch((error) => {
          if (isAuthError(error)) {
            clearAuthenticatedSessionData();
          }
          setConnection(connectionStateForError(error, 1));
          throw error;
        })
        .finally(() => {
          sessionRefreshPromiseRef.current = null;
        });
    }

    return sessionRefreshPromiseRef.current;
  }

  async function withSessionRefresh<T>(
    session: DeviceSession,
    request: (activeSession: DeviceSession) => Promise<T>,
    options: { requireWritable?: boolean } = {},
  ): Promise<T> {
    try {
      return await request(session);
    } catch (error) {
      if (!isAuthError(error)) {
        throw error;
      }
      const refreshed = await refreshActiveSession(session);
      const refreshedConnection = mapHealthToConnection(refreshed.health);
      if (
        options.requireWritable &&
        refreshedConnection.label !== "Connected" &&
        refreshedConnection.label !== "Writable"
      ) {
        throw new ApiError(409, "Bridge is not writable after session refresh");
      }
      return request(refreshed.session);
    }
  }

  function markSessionDataRecovered() {
    setConnection((current) =>
      current.label === "Connection error" || current.label === "Reconnecting" ? { label: "Writable" } : current,
    );
  }

  function updateEventSyncState(
    threadId: string,
    update: (current: SessionEventSyncState) => SessionEventSyncState,
  ): SessionEventSyncState {
    const current = eventSyncByThreadRef.current[threadId] ?? {
      initialized: false,
      hasMoreBefore: false,
      loadingOlder: false,
    };
    const nextState = update(current);
    const nextByThread = {
      ...eventSyncByThreadRef.current,
      [threadId]: nextState,
    };
    eventSyncByThreadRef.current = nextByThread;
    setEventSyncByThread(nextByThread);
    return nextState;
  }

  async function listSessionEventPageWithRefresh(
    session: DeviceSession,
    activeThreadId: string,
    options: SessionEventPageOptions,
  ): Promise<SessionEventPage> {
    return withSessionRefresh(session, (activeSession) =>
      listSessionEvents(
        activeSession.bridgeUrl,
        activeSession.sessionToken,
        activeThreadId,
        options,
      ),
    );
  }

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
              applyHealth(health);
              setDeviceSession(pairedSession);
            }
            return;
          } catch (error) {
            const pairingError = normalizePairingFlowError(error);
            if (!savedSession || !isPairingTokenError(pairingError)) {
              throw pairingError;
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
            applyHealth(health);
            setDeviceSession(nextSession);
          }
          return;
        }

        setConnection({ label: "Connected" });
        const { health, session } = await getHealthWithRefresh(bridgeUrl, savedSession);
        if (!cancelled) {
          applyHealth(health);
          setDeviceSession(session);
        }
      } catch (error) {
        if (cancelled) {
          return;
        }
        const failureState = savedSession
          ? connectionStateForError(error, 1)
          : { label: "Connection error" as const, detail: connectionErrorText(error) };
        if (savedSession && failureState.label === "Reconnecting") {
          setDeviceSession(savedSession);
        }
        setConnection(failureState);
      }
    }

    void loadConnection();

    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!deviceSession || !canSyncSessionData) {
      return;
    }

    let cancelled = false;
    let loading = false;
    let timeoutId: number | null = null;
    const activeSession = deviceSession;

    async function loadSessionList() {
      if (loading) {
        return;
      }
      loading = true;
      try {
        const [items, polledApprovals] = await Promise.all([
          withSessionRefresh(activeSession, (session) =>
            listSessions(session.bridgeUrl, session.sessionToken),
          ),
          withSessionRefresh(activeSession, (session) =>
            listApprovals(session.bridgeUrl, session.sessionToken),
          ).catch(() => null),
        ]);
        if (cancelled) {
          return;
        }
        sessionListFailureCountRef.current = 0;
        markSessionDataRecovered();
        const sorted = sortSessions(items);
        setLiveSessions(sorted);
        if (polledApprovals) {
          setLiveApprovals(polledApprovals);
        }
        setSelectedThreadId((current) => {
          if (sorted.some((session) => session.threadId === current)) {
            return current;
          }
          return sorted[0]?.threadId ?? "";
        });
      } catch (error) {
        if (!cancelled) {
          const failureCount = sessionListFailureCountRef.current + 1;
          sessionListFailureCountRef.current = failureCount;
          setConnection(connectionStateForError(error, failureCount));
        }
      } finally {
        loading = false;
        scheduleNextSessionListLoad();
      }
    }

    function scheduleNextSessionListLoad() {
      if (cancelled) {
        return;
      }
      timeoutId = window.setTimeout(() => {
        void loadSessionList();
      }, nextPollDelay(SESSION_LIST_REFRESH_MS, sessionListFailureCountRef.current));
    }

    function refreshWhenVisible() {
      if (document.visibilityState !== "visible") {
        return;
      }
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
        timeoutId = null;
      }
      void loadSessionList();
    }

    sessionListFailureCountRef.current = 0;
    void loadSessionList();
    document.addEventListener("visibilitychange", refreshWhenVisible);

    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", refreshWhenVisible);
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
      }
    };
  }, [canSyncSessionData, deviceSession]);

  useEffect(() => {
    if (!deviceSession || !sessionDataLoaded || !selectedThreadId) {
      return;
    }

    let cancelled = false;
    let loading = false;
    let timeoutId: number | null = null;
    const activeSession = deviceSession;
    const threadId = selectedThreadId;

    async function loadEvents() {
      if (loading) {
        return;
      }
      loading = true;
      let catchUpImmediately = false;
      try {
        const syncState = eventSyncByThreadRef.current[threadId];
        const page = await listSessionEventPageWithRefresh(
          activeSession,
          threadId,
          syncState?.initialized && syncState.afterCursor
            ? { limit: INCREMENTAL_EVENT_PAGE_LIMIT, since: syncState.afterCursor }
            : { limit: INITIAL_EVENT_PAGE_LIMIT },
        );
        if (!cancelled) {
          sessionEventsFailureCountRef.current = 0;
          markSessionDataRecovered();
          setEventsByThread((current) => ({
            ...current,
            [threadId]: page.legacySnapshot || !syncState?.initialized || page.reset
              ? mergePolledSessionEvents(current[threadId] ?? [], page.events)
              : mergeIncrementalSessionEvents(current[threadId] ?? [], page.events),
          }));
          const canonicalPage = page.legacySnapshot || !syncState?.initialized || page.reset;
          updateEventSyncState(threadId, (currentSync) => ({
            initialized: true,
            beforeCursor: canonicalPage
              ? page.beforeCursor
              : currentSync.beforeCursor ?? page.beforeCursor,
            afterCursor: page.afterCursor ?? currentSync.afterCursor,
            hasMoreBefore: canonicalPage
              ? page.hasMoreBefore
              : currentSync.hasMoreBefore,
            loadingOlder: currentSync.loadingOlder,
          }));
          catchUpImmediately = !page.legacySnapshot && page.hasMoreAfter;
        }
      } catch (error) {
        if (!cancelled) {
          const failureCount = sessionEventsFailureCountRef.current + 1;
          sessionEventsFailureCountRef.current = failureCount;
          setConnection(connectionStateForError(error, failureCount));
        }
      } finally {
        loading = false;
        scheduleNextEventsLoad(catchUpImmediately ? 0 : undefined);
      }
    }

    function scheduleNextEventsLoad(delayOverride?: number) {
      if (cancelled) {
        return;
      }
      timeoutId = window.setTimeout(() => {
        void loadEvents();
      }, delayOverride ?? nextPollDelay(SESSION_EVENTS_REFRESH_MS, sessionEventsFailureCountRef.current));
    }

    function refreshWhenVisible() {
      if (document.visibilityState !== "visible") {
        return;
      }
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
        timeoutId = null;
      }
      void loadEvents();
    }

    sessionEventsFailureCountRef.current = 0;
    void loadEvents();
    document.addEventListener("visibilitychange", refreshWhenVisible);

    return () => {
      cancelled = true;
      document.removeEventListener("visibilitychange", refreshWhenVisible);
      if (timeoutId !== null) {
        window.clearTimeout(timeoutId);
      }
    };
  }, [deviceSession, selectedThreadId, sessionDataLoaded]);

  useEffect(() => {
    if (!deviceSession) {
      return;
    }

    function reconnectWhenVisible() {
      if (document.visibilityState !== "visible") {
        return;
      }
      setSocketReconnectNonce((current) => current + 1);
    }

    function reconnectWhenOnline() {
      setSocketReconnectNonce((current) => current + 1);
    }

    document.addEventListener("visibilitychange", reconnectWhenVisible);
    window.addEventListener("online", reconnectWhenOnline);

    return () => {
      document.removeEventListener("visibilitychange", reconnectWhenVisible);
      window.removeEventListener("online", reconnectWhenOnline);
    };
  }, [deviceSession]);

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
  }, [connection.label, deviceSession, socketReconnectNonce]);

  useEffect(() => {
    draftImagesRef.current = draftImages;
  }, [draftImages]);

  useEffect(() => {
    newSessionImagesRef.current = newSessionImages;
  }, [newSessionImages]);

  useEffect(() => {
    return () => {
      for (const image of [...draftImagesRef.current, ...newSessionImagesRef.current]) {
        URL.revokeObjectURL(image.previewUrl);
      }
    };
  }, []);

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const activeSession = deviceSession;
    const text = draft.trim();
    const images = draftImages;
    if (!canSend || sending || (!text && images.length === 0) || !activeSession || !selectedSession) {
      return;
    }

    const threadId = selectedSession.threadId;
    const localEventId = messageRequestId();
    setSending(true);
    setDraftAttachmentError("");
    setMessageSendError("");
    let outgoingAttachments: OutgoingImageAttachment[];
    try {
      outgoingAttachments = await draftImagesToOutgoingAttachments(images);
    } catch (error) {
      setDraftAttachmentError(error instanceof Error ? error.message : "Unable to read image attachment");
      setSending(false);
      return;
    }

    setEventsByThread((current) => {
      const localEvent: SessionEvent = {
        id: localEventId,
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
    try {
      await sendTextMessage(
        activeSession.bridgeUrl,
        activeSession.sessionToken,
        threadId,
        text,
        outgoingAttachments,
        localEventId,
      );
      setDraft("");
      clearDraftImages();
    } catch (error) {
      setEventsByThread((current) => ({
        ...current,
        [threadId]: (current[threadId] ?? []).filter((sessionEvent) => sessionEvent.id !== localEventId),
      }));
      setMessageSendError(`Message not sent. ${connectionErrorText(error)}`);
      setConnection(connectionStateForError(error, 1));
    } finally {
      setSending(false);
    }
  }

  async function loadNewSessionWorkspaces(
    session: DeviceSession,
    preferredCwd = selectedSession?.cwd,
  ): Promise<void> {
    const requestId = workspaceLoadRequestRef.current + 1;
    workspaceLoadRequestRef.current = requestId;
    setNewSessionWorkspaceError("");
    setNewSessionWorkspacesLoading(true);

    try {
      const workspaces = await withSessionRefresh(session, (activeSession) =>
        listWorkspaces(activeSession.bridgeUrl, activeSession.sessionToken),
      );
      if (workspaceLoadRequestRef.current !== requestId) {
        return;
      }
      setNewSessionError("");
      setNewSessionWorkspaces(workspaces);
      setNewSessionWorkspaceCwd((current) =>
        selectNewSessionWorkspace(workspaces, current, preferredCwd),
      );
    } catch (error) {
      if (workspaceLoadRequestRef.current === requestId) {
        setNewSessionWorkspaces(null);
        setNewSessionWorkspaceError(connectionErrorText(error));
      }
    } finally {
      if (workspaceLoadRequestRef.current === requestId) {
        setNewSessionWorkspacesLoading(false);
      }
    }
  }

  async function handleCreateSession(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const activeSession = deviceSession;
    const text = newSessionDraft.trim();
    const images = newSessionImages;
    const cwd = newSessionWorkspaceCwd;
    if (
      !activeSession ||
      !canCreateSession ||
      sending ||
      createSessionRequestRef.current ||
      (!text && images.length === 0) ||
      !cwd ||
      newSessionWorkspacesLoading ||
      newSessionWorkspaceError
    ) {
      return;
    }

    createSessionRequestRef.current = true;
    setSending(true);
    setNewSessionAttachmentError("");
    setNewSessionError("");
    let outgoingAttachments: OutgoingImageAttachment[];
    try {
      outgoingAttachments = await draftImagesToOutgoingAttachments(images);
    } catch (error) {
      setNewSessionAttachmentError(
        error instanceof Error ? error.message : "Unable to read image attachment",
      );
      createSessionRequestRef.current = false;
      setSending(false);
      return;
    }

    try {
      const snapshot = await withSessionRefresh(activeSession, (session) =>
        createSession(
          session.bridgeUrl,
          session.sessionToken,
          text,
          cwd,
          outgoingAttachments,
        ),
        { requireWritable: true },
      );
      setLiveSessions((current) => sortSessions(upsertSession(current ?? [], snapshot)));
      setSelectedThreadId(snapshot.threadId);
      setEventsByThread((current) => {
        const localEvent: SessionEvent = {
          id: `local-new-session-${Date.now()}`,
          threadId: snapshot.threadId,
          type: "message",
          payload: { role: "user", text, pending: true },
          createdAt: Date.now(),
        };
        return {
          ...current,
          [snapshot.threadId]: sortSessionEvents(
            appendOrMergeSessionEvent(current[snapshot.threadId] ?? [], localEvent),
          ),
        };
      });
      setNewSessionDraft("");
      clearNewSessionImages();
      setIsNewSessionSheetOpen(false);
      markSessionDataRecovered();
    } catch (error) {
      setNewSessionError(connectionErrorText(error));
      if (isWorkspaceError(error)) {
        void loadNewSessionWorkspaces(activeSession);
      }
    } finally {
      createSessionRequestRef.current = false;
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
      setConnection(connectionStateForError(error, 1));
    } finally {
      setDecidingApprovalIds((current) => {
        const next = { ...current };
        delete next[approval.id];
        return next;
      });
    }
  }

  function handleOpenSessionDrawer() {
    setIsSessionDrawerOpen(true);
  }

  function handleOpenNewSession() {
    if (!canCreateSession) {
      return;
    }
    setNewSessionError("");
    setNewSessionWorkspaces(null);
    setNewSessionWorkspaceCwd("");
    setNewSessionWorkspaceError("");
    setIsSessionDrawerOpen(false);
    setIsNewSessionSheetOpen(true);
    if (deviceSession) {
      void loadNewSessionWorkspaces(deviceSession, selectedSession?.cwd);
    }
  }

  function handleCloseNewSession() {
    if (!sending) {
      workspaceLoadRequestRef.current += 1;
      setIsNewSessionSheetOpen(false);
    }
  }

  function handleNewSessionWorkspaceChange(cwd: string) {
    setNewSessionWorkspaceCwd(cwd);
    setNewSessionError("");
  }

  function handleRetryNewSessionWorkspaces() {
    if (deviceSession && !newSessionWorkspacesLoading) {
      void loadNewSessionWorkspaces(deviceSession, selectedSession?.cwd);
    }
  }

  function handleCloseSessionDrawer() {
    setIsSessionDrawerOpen(false);
    sessionMenuButtonRef.current?.focus();
  }

  function handleSelectSession(threadId: string) {
    setSelectedThreadId(threadId);
    handleCloseSessionDrawer();
  }

  function handleAttachDraftImages(files: FileList) {
    const next = appendDraftImageFiles(draftImages, files);
    setDraftAttachmentError(next.error);
    setDraftImages(next.images);
  }

  function handleRemoveDraftImage(id: string) {
    setDraftImages((current) => removeDraftImage(current, id));
    setDraftAttachmentError("");
  }

  function handleAttachNewSessionImages(files: FileList) {
    const next = appendDraftImageFiles(newSessionImages, files);
    setNewSessionAttachmentError(next.error);
    setNewSessionImages(next.images);
  }

  function handleRemoveNewSessionImage(id: string) {
    setNewSessionImages((current) => removeDraftImage(current, id));
    setNewSessionAttachmentError("");
  }

  async function handleLoadOlderEvents(threadId: string): Promise<boolean> {
    const activeSession = deviceSession;
    const syncState = eventSyncByThreadRef.current[threadId];
    if (
      !activeSession ||
      !syncState?.initialized ||
      !syncState.hasMoreBefore ||
      !syncState.beforeCursor ||
      syncState.loadingOlder
    ) {
      return false;
    }

    updateEventSyncState(threadId, (current) => ({ ...current, loadingOlder: true }));
    try {
      const page = await listSessionEventPageWithRefresh(activeSession, threadId, {
        limit: INITIAL_EVENT_PAGE_LIMIT,
        before: syncState.beforeCursor,
      });
      const canonicalPage = page.legacySnapshot || page.reset;
      setEventsByThread((current) => ({
        ...current,
        [threadId]: canonicalPage
          ? mergePolledSessionEvents(current[threadId] ?? [], page.events)
          : mergeIncrementalSessionEvents(current[threadId] ?? [], page.events),
      }));
      updateEventSyncState(threadId, (current) => ({
        initialized: true,
        beforeCursor: canonicalPage
          ? page.beforeCursor
          : page.beforeCursor ?? current.beforeCursor,
        afterCursor: canonicalPage
          ? page.afterCursor
          : current.afterCursor,
        hasMoreBefore: page.hasMoreBefore,
        loadingOlder: false,
      }));
      markSessionDataRecovered();
      return !canonicalPage && page.events.length > 0;
    } catch (error) {
      setConnection(connectionStateForError(error, 1));
      return false;
    } finally {
      updateEventSyncState(threadId, (current) => ({ ...current, loadingOlder: false }));
    }
  }

  function clearDraftImages() {
    setDraftImages((current) => {
      for (const image of current) {
        URL.revokeObjectURL(image.previewUrl);
      }
      return [];
    });
  }

  function clearNewSessionImages() {
    setNewSessionImages((current) => {
      for (const image of current) {
        URL.revokeObjectURL(image.previewUrl);
      }
      return [];
    });
    setNewSessionAttachmentError("");
  }

  return (
    <main className="app-shell" aria-label="Codex mobile workbench">
      <ConnectionBar
        bridgeVersion={bridgeVersion}
        connection={connection}
        newSessionDisabled={!canCreateSession || sending}
        statusText={statusText}
        showSessionMenuButton
        showNewSessionButton
        onCreateSession={handleOpenNewSession}
        sessionMenuButtonRef={sessionMenuButtonRef}
        onOpenSessions={handleOpenSessionDrawer}
      />

      <section className="workbench" aria-label="Workbench">
        {approvals.length > 0 ? (
          <ApprovalQueue
            approvals={approvals}
            decidingApprovalIds={decidingApprovalIds}
            onDecision={handleApprovalDecision}
          />
        ) : null}

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
            hasMoreBefore={selectedEventSync?.hasMoreBefore ?? false}
            loadingOlder={selectedEventSync?.loadingOlder ?? false}
            onLoadOlder={selectedSession
              ? () => handleLoadOlderEvents(selectedSession.threadId)
              : async () => false}
            session={selectedSession}
          />
        </section>
      </section>

      <SessionDrawer
        isOpen={isSessionDrawerOpen}
        newSessionDisabled={!canCreateSession || sending}
        onCreateSession={handleOpenNewSession}
        sessions={sessions}
        selectedThreadId={selectedSession?.threadId ?? ""}
        onClose={handleCloseSessionDrawer}
        onSelect={handleSelectSession}
      />

      <NewSessionSheet
        attachments={newSessionImages}
        attachmentError={newSessionAttachmentError}
        disabled={!canCreateSession || sending}
        draft={newSessionDraft}
        error={newSessionError}
        isOpen={isNewSessionSheetOpen}
        onClose={handleCloseNewSession}
        onAttachFiles={handleAttachNewSessionImages}
        onDraftChange={setNewSessionDraft}
        onRemoveAttachment={handleRemoveNewSessionImage}
        onRetryWorkspaces={handleRetryNewSessionWorkspaces}
        onSubmit={handleCreateSession}
        onWorkspaceChange={handleNewSessionWorkspaceChange}
        submitting={sending}
        workspaceCwd={newSessionWorkspaceCwd}
        workspaceError={newSessionWorkspaceError}
        workspaces={newSessionWorkspaces}
        workspacesLoading={newSessionWorkspacesLoading}
      />

      <Composer
        attachments={draftImages}
        attachmentError={draftAttachmentError}
        draft={draft}
        sendError={messageSendError}
        selectedTitle={selectedSession?.title ?? "No session selected"}
        disabled={!canSend || sending}
        onAttachFiles={handleAttachDraftImages}
        onDraftChange={setDraft}
        onRemoveAttachment={handleRemoveDraftImage}
        onSubmit={handleSubmit}
      />
    </main>
  );
}

function ConnectionBar({
  bridgeVersion,
  connection,
  newSessionDisabled = false,
  onCreateSession,
  onOpenSessions,
  sessionMenuButtonRef,
  showNewSessionButton = false,
  showSessionMenuButton = false,
  statusText,
}: {
  bridgeVersion?: string | null;
  connection: ConnectionViewState;
  newSessionDisabled?: boolean;
  onCreateSession?: () => void;
  onOpenSessions?: () => void;
  sessionMenuButtonRef?: Ref<HTMLButtonElement>;
  showNewSessionButton?: boolean;
  showSessionMenuButton?: boolean;
  statusText: string;
}) {
  const secondaryStatusText = statusText === connection.label ? null : statusText;

  return (
    <header className="connection-bar" aria-label="Connection status">
      {showSessionMenuButton ? (
        <button
          className="session-menu-button"
          onClick={onOpenSessions}
          ref={sessionMenuButtonRef}
          type="button"
          aria-label="Open sessions"
        >
          <Menu size={18} aria-hidden="true" />
        </button>
      ) : null}
      {showNewSessionButton ? (
        <button
          className="new-session-button"
          disabled={newSessionDisabled}
          onClick={onCreateSession}
          type="button"
          aria-label="New session"
        >
          <Plus size={18} aria-hidden="true" />
        </button>
      ) : null}
      <div className="connection-primary">
        <span className={`status-dot ${connectionClass(connection.label)}`} aria-hidden="true" />
        <div>
          <p className="eyebrow">
            LAN bridge
            {bridgeVersion ? <span className="app-version">v{bridgeVersion}</span> : null}
          </p>
          <h1>Codex Mobile</h1>
          {connection.detail ? <p className="connection-detail">{connection.detail}</p> : null}
        </div>
      </div>
      <div className="connection-meta">
        <span className={`meta-chip ${connectionClass(connection.label)}`}>{connection.label}</span>
        {secondaryStatusText ? <span className="meta-chip muted">{secondaryStatusText}</span> : null}
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
  newSessionDisabled,
  onClose,
  onCreateSession,
  onSelect,
  selectedThreadId,
  sessions,
}: {
  isOpen: boolean;
  newSessionDisabled: boolean;
  onClose: () => void;
  onCreateSession: () => void;
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
            <button
              className="drawer-new-button"
              disabled={newSessionDisabled}
              onClick={onCreateSession}
              type="button"
            >
              <Plus size={14} aria-hidden="true" />
              New
            </button>
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

function NewSessionSheet({
  attachments,
  attachmentError,
  disabled,
  draft,
  error,
  isOpen,
  onClose,
  onAttachFiles,
  onDraftChange,
  onRemoveAttachment,
  onRetryWorkspaces,
  onSubmit,
  onWorkspaceChange,
  submitting,
  workspaceCwd,
  workspaceError,
  workspaces,
  workspacesLoading,
}: {
  attachments: DraftImageAttachment[];
  attachmentError: string;
  disabled: boolean;
  draft: string;
  error: string;
  isOpen: boolean;
  onClose: () => void;
  onAttachFiles: (files: FileList) => void;
  onDraftChange: (value: string) => void;
  onRemoveAttachment: (id: string) => void;
  onRetryWorkspaces: () => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  onWorkspaceChange: (cwd: string) => void;
  submitting: boolean;
  workspaceCwd: string;
  workspaceError: string;
  workspaces: WorkspaceOption[] | null;
  workspacesLoading: boolean;
}) {
  const textareaRef = useRef<HTMLTextAreaElement | null>(null);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    textareaRef.current?.focus();
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
    <div className="new-session-layer">
      <button className="new-session-backdrop" onClick={onClose} type="button" aria-label="Close new session" />
      <section className="new-session-sheet" role="dialog" aria-modal="true" aria-labelledby="new-session-heading">
        <div className="sheet-heading">
          <div>
            <p className="eyebrow">New thread</p>
            <h2 id="new-session-heading">Start from phone</h2>
          </div>
          <button className="icon-button" onClick={onClose} type="button" aria-label="Close new session">
            <X size={16} aria-hidden="true" />
          </button>
        </div>
        <form className="new-session-form" onSubmit={onSubmit}>
          <div className="new-session-workspace-field">
            <label htmlFor="new-session-workspace">Workspace</label>
            {workspaces ? (
              workspaces.length > 0 ? (
                <select
                  disabled={workspacesLoading || submitting}
                  id="new-session-workspace"
                  onChange={(event) => onWorkspaceChange(event.target.value)}
                  value={workspaceCwd}
                >
                  <option value="">Select a workspace</option>
                  {workspaces.map((workspace) => (
                    <option key={workspace.cwd} value={workspace.cwd}>
                      {workspace.cwd}
                    </option>
                  ))}
                </select>
              ) : (
                <div className="workspace-empty-state" role="status">
                  No safe workspaces are available from existing Codex sessions.
                </div>
              )
            ) : workspacesLoading ? (
              <div className="workspace-loading-state" role="status">
                <RefreshCw size={15} aria-hidden="true" />
                Loading workspaces...
              </div>
            ) : null}
            {workspaceError ? (
              <div className="sheet-error workspace-error" role="alert">
                <span>
                  <AlertTriangle size={15} aria-hidden="true" />
                  {workspaceError}
                </span>
                <button disabled={workspacesLoading} onClick={onRetryWorkspaces} type="button">
                  <RefreshCw size={14} aria-hidden="true" />
                  Retry workspaces
                </button>
              </div>
            ) : null}
          </div>
          <label className="sr-only" htmlFor="new-session-message">
            First message for new session
          </label>
          <DraftImagePreviews
            attachments={attachments}
            attachmentError={attachmentError}
            disabled={disabled}
            onRemoveAttachment={onRemoveAttachment}
          />
          <div className="new-session-message-row">
            <ImageAttachmentButton
              buttonLabel="Attach image to new session"
              disabled={disabled}
              inputLabel="Choose new session image attachment"
              onAttachFiles={onAttachFiles}
            />
            <textarea
              disabled={disabled}
              id="new-session-message"
              onChange={(event) => onDraftChange(event.target.value)}
              placeholder="Describe the task Codex should start"
              ref={textareaRef}
              rows={4}
              value={draft}
            />
          </div>
          {error ? (
            <div className="sheet-error" role="alert">
              <AlertTriangle size={15} aria-hidden="true" />
              {error}
            </div>
          ) : null}
          <button
            className="create-session-button"
            disabled={
              disabled ||
              (!draft.trim() && attachments.length === 0) ||
              !workspaceCwd ||
              workspacesLoading ||
              Boolean(workspaceError)
            }
            type="submit"
          >
            {submitting ? "Creating..." : "Create & send"}
          </button>
        </form>
      </section>
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
  hasMoreBefore,
  loadingOlder,
  onLoadOlder,
  session,
}: {
  assetSession: DeviceSession | null;
  approvals: ApprovalRequest[];
  events: SessionEvent[];
  hasMoreBefore: boolean;
  loadingOlder: boolean;
  onLoadOlder: () => Promise<boolean>;
  session: SessionSnapshot | null;
}) {
  const eventStreamRef = useRef<HTMLDivElement | null>(null);
  const shouldStickToBottomRef = useRef(true);
  const loadingOlderRequestRef = useRef(false);
  const prependScrollRef = useRef<{
    firstEventId?: string;
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);
  const previousThreadIdRef = useRef<string | null>(null);
  const threadId = session ? session.threadId : "";
  const eventHead = sessionEvents[0];
  const eventHeadKey = eventHead?.id ?? "";
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

    const pendingPrepend = prependScrollRef.current;
    if (pendingPrepend && pendingPrepend.firstEventId !== eventHeadKey) {
      stream.scrollTop = pendingPrepend.scrollTop + (stream.scrollHeight - pendingPrepend.scrollHeight);
      prependScrollRef.current = null;
      shouldStickToBottomRef.current = false;
      previousThreadIdRef.current = threadId;
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
  }, [eventHeadKey, eventTailKey, sessionEvents.length, threadId]);

  async function requestOlderEvents() {
    const stream = eventStreamRef.current;
    if (!stream || !hasMoreBefore || loadingOlder || loadingOlderRequestRef.current) {
      return;
    }
    loadingOlderRequestRef.current = true;
    prependScrollRef.current = {
      firstEventId: eventHeadKey || undefined,
      scrollHeight: stream.scrollHeight,
      scrollTop: stream.scrollTop,
    };
    try {
      const added = await onLoadOlder();
      if (!added) {
        prependScrollRef.current = null;
      }
    } finally {
      loadingOlderRequestRef.current = false;
    }
  }

  function handleEventStreamScroll() {
    const stream = eventStreamRef.current;
    if (!stream) {
      return;
    }
    shouldStickToBottomRef.current =
      stream.scrollHeight - stream.scrollTop - stream.clientHeight < 80;
    if (stream.scrollTop < 48 && hasMoreBefore && !loadingOlder) {
      void requestOlderEvents();
    }
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
        <div className="detail-title-wrap">
          <h2 id="session-detail-heading">{session.title}</h2>
        </div>
        <StatusBadge status={session.status} />
        <div className="compact-session-meta" aria-label="Session metadata">
          <span>{session.cwd ?? "Unknown workspace"}</span>
          <span className="dot-separator" aria-hidden="true">/</span>
          <span className="model-meta">{session.modelProvider ?? "Unset model"}</span>
        </div>
      </div>

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
        {hasMoreBefore ? (
          <div className="event-history-loader">
            <button
              disabled={loadingOlder}
              onClick={() => void requestOlderEvents()}
              type="button"
            >
              <Clock3 size={14} aria-hidden="true" />
              {loadingOlder ? "Loading earlier messages" : "Load earlier messages"}
            </button>
          </div>
        ) : null}
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
  const actor = eventActor(event);

  return (
    <article className={`event-row ${actor}`}>
      <span className="event-icon" aria-hidden="true">
        {eventIcon(event, actor)}
      </span>
      <div className="event-content">
        <div className="event-meta">
          <p className="event-kind">{eventKindLabel(event, actor)}</p>
          <time dateTime={new Date(event.createdAt).toISOString()}>{formatEventTime(event.createdAt)}</time>
        </div>
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

type EventActor = "assistant" | "system" | "user";

function eventActor(event: SessionEvent): EventActor {
  if (event.type !== "message" || !event.payload || typeof event.payload !== "object" || Array.isArray(event.payload)) {
    return "system";
  }

  const role = (event.payload as Record<string, unknown>).role;
  if (role === "user" || role === "assistant") {
    return role;
  }
  return "system";
}

function eventIcon(event: SessionEvent, actor: EventActor) {
  if (actor === "user") {
    return <UserRound size={14} />;
  }
  if (actor === "assistant") {
    return <Bot size={14} />;
  }
  return event.type === "tool_call" ? <TerminalSquare size={14} /> : <Clock3 size={14} />;
}

function eventKindLabel(event: SessionEvent, actor: EventActor) {
  if (actor === "user") {
    return "You";
  }
  if (actor === "assistant") {
    return "Codex";
  }
  return event.type.replace("_", " ");
}

function formatEventTime(timestamp: number) {
  const date = new Date(timestamp);
  const now = new Date();
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();

  return new Intl.DateTimeFormat(undefined, {
    month: sameDay ? undefined : "numeric",
    day: sameDay ? undefined : "numeric",
    hour: "2-digit",
    hourCycle: "h23",
    minute: "2-digit",
  }).format(date);
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
  const [shouldLoad, setShouldLoad] = useState(() => typeof IntersectionObserver === "undefined");
  const placeholderRef = useRef<HTMLSpanElement | null>(null);
  const bridgeUrl = assetSession?.bridgeUrl ?? "";
  const sessionToken = assetSession?.sessionToken ?? "";

  useEffect(() => {
    if (shouldLoad || typeof IntersectionObserver === "undefined") {
      return;
    }
    const placeholder = placeholderRef.current;
    if (!placeholder) {
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setShouldLoad(true);
          observer.disconnect();
        }
      },
      { rootMargin: "240px 0px" },
    );
    observer.observe(placeholder);
    return () => observer.disconnect();
  }, [shouldLoad]);

  useEffect(() => {
    let cancelled = false;
    let createdObjectUrl: string | null = null;

    setObjectUrl(null);
    setFailed(false);

    if (!shouldLoad) {
      return;
    }

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
  }, [bridgeUrl, sessionToken, attachment.src, shouldLoad]);

  if (failed) {
    return <span className="attachment-error" role="status">Image unavailable: {attachment.name}</span>;
  }

  if (!objectUrl) {
    return (
      <span className="attachment-loading" ref={placeholderRef} role="status">
        {shouldLoad ? "Loading image" : "Image queued"}: {attachment.name}
      </span>
    );
  }

  return (
    <img
      className="attachment-image"
      decoding="async"
      loading="lazy"
      src={objectUrl}
      alt={attachment.name}
    />
  );
}

function DraftImagePreviews({
  attachments,
  attachmentError,
  disabled,
  onRemoveAttachment,
}: {
  attachments: DraftImageAttachment[];
  attachmentError: string;
  disabled: boolean;
  onRemoveAttachment: (id: string) => void;
}) {
  return (
    <>
      {attachments.length > 0 ? (
        <div className="composer-attachments" aria-label="Selected image attachments">
          {attachments.map((attachment) => (
            <span className="composer-attachment" key={attachment.id}>
              <img src={attachment.previewUrl} alt={attachment.name} />
              <button
                className="composer-attachment-remove"
                disabled={disabled}
                onClick={() => onRemoveAttachment(attachment.id)}
                type="button"
                aria-label={`Remove ${attachment.name}`}
                title={`Remove ${attachment.name}`}
              >
                <X size={12} aria-hidden="true" />
              </button>
            </span>
          ))}
        </div>
      ) : null}
      {attachmentError ? (
        <div className="composer-error" role="alert">
          <AlertTriangle size={14} aria-hidden="true" />
          {attachmentError}
        </div>
      ) : null}
    </>
  );
}

function ImageAttachmentButton({
  buttonLabel,
  disabled,
  inputLabel,
  onAttachFiles,
}: {
  buttonLabel: string;
  disabled: boolean;
  inputLabel: string;
  onAttachFiles: (files: FileList) => void;
}) {
  const fileInputRef = useRef<HTMLInputElement | null>(null);

  return (
    <>
      <input
        accept="image/png,image/jpeg,image/gif,image/webp"
        aria-label={inputLabel}
        className="sr-only"
        disabled={disabled}
        multiple
        onChange={(event) => {
          const files = event.currentTarget.files;
          if (files) {
            onAttachFiles(files);
          }
          event.currentTarget.value = "";
        }}
        ref={fileInputRef}
        type="file"
      />
      <button
        className="attach-button"
        disabled={disabled}
        onClick={() => fileInputRef.current?.click()}
        type="button"
        aria-label={buttonLabel}
        title={buttonLabel}
      >
        <ImagePlus size={17} aria-hidden="true" />
      </button>
    </>
  );
}

function Composer({
  attachments,
  attachmentError,
  disabled,
  draft,
  onAttachFiles,
  onDraftChange,
  onRemoveAttachment,
  onSubmit,
  sendError,
  selectedTitle,
}: {
  attachments: DraftImageAttachment[];
  attachmentError: string;
  disabled: boolean;
  draft: string;
  onAttachFiles: (files: FileList) => void;
  onDraftChange: (value: string) => void;
  onRemoveAttachment: (id: string) => void;
  onSubmit: (event: FormEvent<HTMLFormElement>) => void;
  sendError: string;
  selectedTitle: string;
}) {
  const canSubmit = !disabled && (draft.trim().length > 0 || attachments.length > 0);

  return (
    <form className="composer" aria-label="Message composer" onSubmit={onSubmit}>
      <DraftImagePreviews
        attachments={attachments}
        attachmentError={attachmentError}
        disabled={disabled}
        onRemoveAttachment={onRemoveAttachment}
      />
      {sendError ? (
        <div className="composer-error" role="alert">
          <AlertTriangle size={14} aria-hidden="true" />
          {sendError}
        </div>
      ) : null}
      <label className="sr-only" htmlFor="codex-message">
        Message selected Codex session
      </label>
      <ImageAttachmentButton
        buttonLabel="Attach image"
        disabled={disabled}
        inputLabel="Choose image attachment"
        onAttachFiles={onAttachFiles}
      />
      <textarea
        id="codex-message"
        name="message"
        onChange={(event) => onDraftChange(event.target.value)}
        placeholder={`Message ${selectedTitle}`}
        rows={1}
        disabled={disabled}
        value={draft}
      />
      <button className="send-button" type="submit" aria-label="Send message" disabled={!canSubmit}>
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

function appendDraftImageFiles(
  current: DraftImageAttachment[],
  files: FileList,
): { images: DraftImageAttachment[]; error: string } {
  const incoming = Array.from(files);
  if (incoming.length === 0) {
    return { images: current, error: "" };
  }

  const images = [...current];
  let error = "";
  for (const file of incoming) {
    if (images.length >= MAX_DRAFT_IMAGE_ATTACHMENTS) {
      error = `Up to ${MAX_DRAFT_IMAGE_ATTACHMENTS} images per message`;
      break;
    }
    if (!SUPPORTED_DRAFT_IMAGE_TYPES.has(file.type)) {
      error = "Only PNG, JPEG, GIF, or WebP images are supported";
      continue;
    }
    if (file.size > MAX_DRAFT_IMAGE_BYTES) {
      error = "Image must be 8 MB or smaller";
      continue;
    }
    images.push({
      id: draftImageId(),
      file,
      name: file.name || "image",
      mimeType: file.type,
      previewUrl: URL.createObjectURL(file),
      size: file.size,
    });
  }

  return { images, error };
}

function removeDraftImage(images: DraftImageAttachment[], id: string): DraftImageAttachment[] {
  const removed = images.find((image) => image.id === id);
  if (removed) {
    URL.revokeObjectURL(removed.previewUrl);
  }
  return images.filter((image) => image.id !== id);
}

async function draftImagesToOutgoingAttachments(images: DraftImageAttachment[]): Promise<OutgoingImageAttachment[]> {
  return Promise.all(
    images.map(async (image) => ({
      name: image.name,
      mimeType: image.mimeType,
      dataBase64: await fileToBase64(image.file),
    })),
  );
}

function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error("Unable to read image attachment"));
    reader.onload = () => {
      const result = reader.result;
      if (typeof result !== "string") {
        reject(new Error("Unable to read image attachment"));
        return;
      }
      resolve(result.split(",").at(1) ?? "");
    };
    reader.readAsDataURL(file);
  });
}

function draftImageId() {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `draft-image-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function messageRequestId() {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `message-${Date.now()}-${Math.random().toString(36).slice(2)}`;
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
    const transientIndex = events.findIndex((existing) =>
      shouldReconcileUserMessages(existing, event),
    );
    if (transientIndex !== -1) {
      if (isPendingPayload(event.payload) && isBridgeUserEcho(events[transientIndex])) {
        return events;
      }
      const nextEvents = [...events];
      nextEvents[transientIndex] = event;
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
      .filter((text): text is string => text !== null),
  );
  const carried = current.filter((event) => {
    if (!isPendingPayload(event.payload)) {
      return false;
    }
    const text = normalizedUserMessageText(event);
    if (text !== null && polledUserTexts.has(text)) {
      return false;
    }
    return true;
  });

  return sortSessionEvents(mergeSessionEvents([...polled, ...carried]));
}

export function mergeIncrementalSessionEvents(
  current: SessionEvent[],
  incremental: SessionEvent[],
): SessionEvent[] {
  const incrementalUserTexts = new Set(
    incremental
      .map(normalizedUserMessageText)
      .filter((text): text is string => text !== null),
  );
  const retained = current.filter((event) => {
    if (!isPendingPayload(event.payload)) {
      return true;
    }
    const text = normalizedUserMessageText(event);
    return text === null || !incrementalUserTexts.has(text);
  });

  return sortSessionEvents(mergeSessionEvents([...retained, ...incremental]));
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

function shouldReconcileUserMessages(existing: SessionEvent, next: SessionEvent): boolean {
  if (!isUserMessage(existing) || !isUserMessage(next)) {
    return false;
  }
  const nextText = normalizedUserMessageText(next);
  if (existing.threadId !== next.threadId || nextText === null) {
    return false;
  }
  if (normalizedUserMessageText(existing) !== nextText) {
    return false;
  }

  const existingPending = isPendingPayload(existing.payload);
  const nextPending = isPendingPayload(next.payload);
  const existingBridgeEcho = isBridgeUserEcho(existing);

  if (existingPending) {
    return !nextPending;
  }
  if (!existingBridgeEcho) {
    return false;
  }
  return Math.abs(existing.createdAt - next.createdAt) <= USER_MESSAGE_RECONCILIATION_WINDOW_MS;
}

function isBridgeUserEcho(event: SessionEvent): boolean {
  if (!isUserMessage(event)) {
    return false;
  }
  const payload = event.payload;
  if (
    payload !== null &&
    typeof payload === "object" &&
    !Array.isArray(payload) &&
    payload.bridgeEcho === true
  ) {
    return true;
  }
  return BRIDGE_EVENT_ID_PATTERN.test(event.id);
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

const BRIDGE_EVENT_ID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const USER_MESSAGE_RECONCILIATION_WINDOW_MS = 5 * 60 * 1_000;

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

function connectionErrorText(error: unknown): string {
  if (isAuthError(error)) {
    return "Session revoked or expired";
  }
  if (
    error instanceof ApiError &&
    error.status === 400 &&
    (error.code === "invalid_pairing_token" || error.code === "expired_pairing_token")
  ) {
    return "Pairing link expired or already used. Restart the bridge and open the newest pairing URL.";
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "Unable to reach bridge";
}

export function connectionStateForError(error: unknown, failureCount: number): ConnectionViewState {
  const detail = connectionErrorText(error);
  if (!isTransientConnectionError(error)) {
    return { label: "Connection error", detail };
  }
  if (failureCount < TRANSIENT_FAILURES_BEFORE_NEW_LINK) {
    return { label: "Reconnecting", detail };
  }
  return {
    label: "Connection error",
    detail: `${detail}. The public link has failed repeatedly; open the newest link from the Mac.`,
  };
}

function isTransientConnectionError(error: unknown): boolean {
  if (error instanceof ApiError) {
    return error.status === 408 || error.status === 429 || error.status >= 500;
  }
  return error instanceof TypeError;
}

function isAuthError(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 401 || error.status === 403);
}

function isPairingTokenError(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    error.status === 400 &&
    (error.code === "invalid_pairing_token" || error.code === "expired_pairing_token")
  );
}

function normalizePairingFlowError(error: unknown): unknown {
  if (error instanceof ApiError && error.status === 400 && !error.code) {
    return new ApiError(error.status, error.message, "invalid_pairing_token");
  }
  return error;
}

function isWorkspaceError(error: unknown): boolean {
  return (
    error instanceof ApiError &&
    (error.code === "workspace_required" ||
      error.code === "workspace_not_allowed" ||
      error.code === "workspace_unavailable")
  );
}

export function nextPollDelay(
  baseMs: number,
  failureCount: number,
  visibilityState: DocumentVisibilityState = document.visibilityState,
): number {
  const hiddenMultiplier = visibilityState === "hidden" ? HIDDEN_PAGE_POLL_MULTIPLIER : 1;
  const backoffMultiplier = Math.min(2 ** Math.max(0, failureCount), 8);
  return Math.min(baseMs * hiddenMultiplier * backoffMultiplier, MAX_POLL_BACKOFF_MS);
}

function sortSessions(items: SessionSnapshot[]): SessionSnapshot[] {
  return [...items].sort((left, right) => right.updatedAt - left.updatedAt);
}

function selectNewSessionWorkspace(
  workspaces: WorkspaceOption[],
  currentCwd: string,
  preferredCwd?: string,
): string {
  if (currentCwd && workspaces.some((workspace) => workspace.cwd === currentCwd)) {
    return currentCwd;
  }
  if (preferredCwd && workspaces.some((workspace) => workspace.cwd === preferredCwd)) {
    return preferredCwd;
  }
  return workspaces.length === 1 ? workspaces[0].cwd : "";
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
    case "Reconnecting":
      return "warn";
    case "Unpaired":
      return "muted";
    default:
      return "error";
  }
}

export default App;
