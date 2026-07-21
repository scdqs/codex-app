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
  ChevronDown,
  ChevronRight,
  CircleDot,
  Clock3,
  Command,
  FilePenLine,
  FileText,
  FolderOpen,
  GitBranch,
  Globe2,
  Hammer,
  Hourglass,
  Image as ImageIcon,
  ImagePlus,
  Menu,
  Pin,
  Plus,
  RefreshCw,
  Search,
  Send,
  Settings as SettingsIcon,
  ShieldAlert,
  TerminalSquare,
  UserRound,
  UsersRound,
  Wrench,
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
  type AlertEvent,
  type AlertKind,
  type ApprovalKind,
  type ApprovalRequest,
  type DecisionKind,
  type ImageAttachment,
  type ServerEnvelope,
  type SessionEvent,
  type SessionSnapshot,
  type SessionStatus,
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
import { groupSessionsByProject } from "./project-view";
import { eventTurnScope, groupSessionEventsForDisplay } from "./turn-groups";
import {
  createDeviceSession,
  loadProjectViewPreferences,
  loadSession,
  saveProjectViewPreferences,
  saveSession,
  type DeviceSession,
  type ProjectViewPreferences,
} from "./storage";
import {
  deletePushSubscription,
  getNotificationSettings,
  getPushPublicKey,
  putNotificationSettings,
  savePushSubscription,
  sendTestAlert,
  type DeviceNotificationSettings,
  type NotificationCapabilities,
  type NotificationSettingsResponse,
  type PushSubscriptionState,
} from "./notifications/api";
import {
  browserForegroundCapabilities,
  type ForegroundCapabilities,
} from "./notifications/capabilities";
import {
  createForegroundAlertPlayer,
  type ForegroundAlertPlayer,
} from "./notifications/foreground-alert-player";
import { NotificationSettingsPage } from "./notifications/NotificationSettingsPage";
import { NotificationOnboardingSheet } from "./notifications/NotificationOnboardingSheet";
import {
  dismissNotificationOnboarding,
  hasDismissedNotificationOnboarding,
} from "./notifications/onboarding-storage";
import {
  PushSubscriptionController,
  type SystemNotificationState,
} from "./notifications/push-subscription-controller";
import {
  isAlertClientMessage,
  isOpenThreadMessage,
} from "./notifications/push-protocol";

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
const DEFAULT_NOTIFICATION_SETTINGS: DeviceNotificationSettings = {
  enabled: false,
  alertKinds: {
    completed: true,
    approvalRequired: true,
    inputRequired: true,
    error: true,
  },
  soundEnabled: true,
  vibrationEnabled: true,
};
const DEFAULT_NOTIFICATION_CAPABILITIES: NotificationCapabilities = {
  deliveryMode: "foreground_only",
  fixedHttps: false,
  systemNotifications: false,
  foregroundSound: typeof window !== "undefined" && ("AudioContext" in window || "webkitAudioContext" in window),
  foregroundVibration: typeof navigator !== "undefined" && typeof navigator.vibrate === "function",
  vibrationControlledBySystem: false,
};

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
  const [view, setView] = useState<"workbench" | "settings">("workbench");
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
  const [notificationSettings, setNotificationSettings] = useState<DeviceNotificationSettings>(
    DEFAULT_NOTIFICATION_SETTINGS,
  );
  const [notificationCapabilities, setNotificationCapabilities] = useState<NotificationCapabilities>(
    DEFAULT_NOTIFICATION_CAPABILITIES,
  );
  const [browserNotificationCapabilities, setBrowserNotificationCapabilities] =
    useState<ForegroundCapabilities>(() => browserForegroundCapabilities(false));
  const [pushSubscriptionState, setPushSubscriptionState] = useState<PushSubscriptionState>(
    "unavailable",
  );
  const [systemNotificationState, setSystemNotificationState] =
    useState<SystemNotificationState>("unavailable");
  const [notificationBusy, setNotificationBusy] = useState(false);
  const [notificationError, setNotificationError] = useState("");
  const [showNotificationOnboarding, setShowNotificationOnboarding] = useState(false);
  const [soundBlocked, setSoundBlocked] = useState(false);
  const [pendingNotificationThreadId, setPendingNotificationThreadId] = useState<string | null>(
    readNotificationThreadIdFromUrl,
  );
  const [notificationDeepLinkError, setNotificationDeepLinkError] = useState("");
  const [projectViewPreferences, setProjectViewPreferences] = useState<ProjectViewPreferences>(
    loadProjectViewPreferences,
  );
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
  const notificationSettingsRef = useRef(notificationSettings);
  const alertPlayerRef = useRef<ForegroundAlertPlayer | null>(null);
  if (!alertPlayerRef.current) {
    alertPlayerRef.current = createForegroundAlertPlayer();
  }
  const canSyncSessionData =
    Boolean(deviceSession) &&
    (isSessionDataEnabled(connection.label) ||
      connection.label === "Connection error" ||
      connection.label === "Pairing");
  const sessionDataLoaded = liveSessions !== null;
  const sessions = useMemo(
    () => userVisibleSessions(liveSessions ?? []),
    [liveSessions],
  );
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
  const collapsedProjectIds = useMemo(
    () => new Set(projectViewPreferences.collapsedProjectIds),
    [projectViewPreferences.collapsedProjectIds],
  );
  const pinnedThreadIds = useMemo(
    () => new Set(projectViewPreferences.pinnedThreadIds),
    [projectViewPreferences.pinnedThreadIds],
  );

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
    setSystemNotificationState("unavailable");
    setPushSubscriptionState("unavailable");
  }

  function updateProjectViewPreferences(
    update: (current: ProjectViewPreferences) => ProjectViewPreferences,
  ) {
    setProjectViewPreferences((current) => {
      const next = update(current);
      saveProjectViewPreferences(next);
      return next;
    });
  }

  function handleToggleProject(projectId: string) {
    updateProjectViewPreferences((current) => {
      const collapsed = new Set(current.collapsedProjectIds);
      if (collapsed.has(projectId)) {
        collapsed.delete(projectId);
      } else {
        collapsed.add(projectId);
      }
      return { ...current, collapsedProjectIds: [...collapsed] };
    });
  }

  function handleTogglePinnedThread(threadId: string) {
    updateProjectViewPreferences((current) => {
      const pinned = new Set(current.pinnedThreadIds);
      if (pinned.has(threadId)) {
        pinned.delete(threadId);
      } else {
        pinned.add(threadId);
      }
      return { ...current, pinnedThreadIds: [...pinned] };
    });
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
        const visible = userVisibleSessions(sorted);
        setLiveSessions(sorted);
        if (polledApprovals) {
          setLiveApprovals(polledApprovals);
        }
        setSelectedThreadId((current) => {
          if (visible.some((session) => session.threadId === current)) {
            return current;
          }
          return preferredInitialSessionId(visible);
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
    notificationSettingsRef.current = notificationSettings;
  }, [notificationSettings]);

  useEffect(() => {
    if (!deviceSession) {
      setShowNotificationOnboarding(false);
      return;
    }
    let cancelled = false;
    void getNotificationSettings(deviceSession)
      .then(async (response) => {
        if (cancelled) {
          return;
        }
        const browserCapabilities = browserForegroundCapabilities(response.capabilities.fixedHttps);
        setNotificationSettings(response.settings);
        setPushSubscriptionState(response.subscriptionState);
        setBrowserNotificationCapabilities(browserCapabilities);
        setNotificationCapabilities({
          ...response.capabilities,
          foregroundSound:
            response.capabilities.foregroundSound && browserCapabilities.foregroundSound,
          foregroundVibration:
            response.capabilities.foregroundVibration && browserCapabilities.foregroundVibration,
        });
        const systemState = await createPushSubscriptionController(deviceSession).inspect(
          browserCapabilities,
          response.subscriptionState,
        );
        if (cancelled) {
          return;
        }
        setSystemNotificationState(systemState);
        setNotificationError("");
        setShowNotificationOnboarding(
          !hasDismissedNotificationOnboarding(deviceSession.deviceId),
        );
      })
      .catch((error) => {
        if (!cancelled) {
          setNotificationError(
            error instanceof Error ? error.message : "Unable to load notification settings",
          );
        }
      });
    return () => {
      cancelled = true;
    };
  }, [deviceSession]);

  useEffect(() => {
    if (!("serviceWorker" in navigator)) {
      return;
    }
    function handleServiceWorkerMessage(event: MessageEvent) {
      if (isAlertClientMessage(event.data)) {
        handleForegroundAlert(
          event.data.payload,
          alertPlayerRef.current,
          notificationSettingsRef.current,
          setSoundBlocked,
        );
        return;
      }
      if (isOpenThreadMessage(event.data)) {
        setNotificationDeepLinkError("");
        setPendingNotificationThreadId(event.data.threadId);
      }
    }
    navigator.serviceWorker.addEventListener("message", handleServiceWorkerMessage);
    return () => {
      navigator.serviceWorker.removeEventListener("message", handleServiceWorkerMessage);
    };
  }, []);

  useEffect(() => {
    if (!liveSessions || !pendingNotificationThreadId) {
      return;
    }
    const target = sessions.find(
      (session) => session.threadId === pendingNotificationThreadId,
    );
    if (target) {
      setSelectedThreadId(target.threadId);
      setView("workbench");
      setIsSessionDrawerOpen(false);
      setNotificationDeepLinkError("");
    } else {
      setNotificationDeepLinkError("Session is no longer available");
    }
    setPendingNotificationThreadId(null);
    clearNotificationThreadParamFromUrl();
  }, [pendingNotificationThreadId, sessions]);

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
      handleServerEnvelope(
        envelope,
        setLiveSessions,
        setEventsByThread,
        setLiveApprovals,
        (alert) => {
          handleForegroundAlert(
            alert,
            alertPlayerRef.current,
            notificationSettingsRef.current,
            setSoundBlocked,
          );
        },
      );
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
    setNotificationDeepLinkError("");
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

  async function handleSaveNotificationSettings(next: DeviceNotificationSettings) {
    if (!deviceSession || notificationBusy) {
      return;
    }
    setNotificationBusy(true);
    setNotificationError("");
    try {
      const response = await putNotificationSettings(deviceSession, next);
      await applyNotificationResponse(deviceSession, response);
    } catch (error) {
      setNotificationError(
        error instanceof Error ? error.message : "Unable to save notification settings",
      );
    } finally {
      setNotificationBusy(false);
    }
  }

  async function applyNotificationResponse(
    activeSession: DeviceSession,
    response: NotificationSettingsResponse,
  ) {
    const browserCapabilities = browserForegroundCapabilities(response.capabilities.fixedHttps);
    setNotificationSettings(response.settings);
    setPushSubscriptionState(response.subscriptionState);
    setBrowserNotificationCapabilities(browserCapabilities);
    setNotificationCapabilities({
      ...response.capabilities,
      foregroundSound:
        response.capabilities.foregroundSound && browserCapabilities.foregroundSound,
      foregroundVibration:
        response.capabilities.foregroundVibration && browserCapabilities.foregroundVibration,
    });
    setSystemNotificationState(
      await createPushSubscriptionController(activeSession).inspect(
        browserCapabilities,
        response.subscriptionState,
      ),
    );
  }

  function handleOpenSettings() {
    setIsSessionDrawerOpen(false);
    setView("settings");
  }

  async function handlePreviewAlert(kind: AlertKind) {
    try {
      await alertPlayerRef.current?.preview(kind);
      setSoundBlocked(false);
    } catch {
      setSoundBlocked(true);
    }
  }

  async function handleSendTestAlert() {
    if (!deviceSession) {
      return;
    }
    setNotificationBusy(true);
    setNotificationError("");
    try {
      await alertPlayerRef.current?.unlock();
      await sendTestAlert(deviceSession);
    } catch (error) {
      setNotificationError(error instanceof Error ? error.message : "Unable to send test alert");
    } finally {
      setNotificationBusy(false);
    }
  }

  async function handleEnableSystemNotifications() {
    const activeSession = deviceSession;
    if (!activeSession || notificationBusy) {
      return;
    }
    setNotificationBusy(true);
    setNotificationError("");
    try {
      try {
        await alertPlayerRef.current?.unlock();
      } catch {
        setSoundBlocked(true);
      }
      const controller = createPushSubscriptionController(activeSession);
      await controller.enable(browserNotificationCapabilities);
      const response = await putNotificationSettings(activeSession, {
        ...notificationSettings,
        enabled: true,
      });
      await applyNotificationResponse(activeSession, response);
      await sendTestAlert(activeSession);
    } catch (error) {
      setNotificationError(notificationActionError(error));
      setSystemNotificationState(
        await createPushSubscriptionController(activeSession).inspect(
          browserNotificationCapabilities,
          pushSubscriptionState,
        ),
      );
    } finally {
      setNotificationBusy(false);
    }
  }

  async function handleRepairSystemNotifications() {
    const activeSession = deviceSession;
    if (!activeSession || notificationBusy) {
      return;
    }
    setNotificationBusy(true);
    setNotificationError("");
    try {
      const controller = createPushSubscriptionController(activeSession);
      await controller.repair(browserNotificationCapabilities);
      const response = await putNotificationSettings(activeSession, {
        ...notificationSettings,
        enabled: true,
      });
      await applyNotificationResponse(activeSession, response);
      await sendTestAlert(activeSession);
    } catch (error) {
      setNotificationError(notificationActionError(error));
      setSystemNotificationState("needs_repair");
    } finally {
      setNotificationBusy(false);
    }
  }

  async function handleDisableAlerts() {
    const activeSession = deviceSession;
    if (!activeSession || notificationBusy) {
      return;
    }
    setNotificationBusy(true);
    setNotificationError("");
    try {
      const response = await putNotificationSettings(activeSession, {
        ...notificationSettings,
        enabled: false,
      });
      await applyNotificationResponse(activeSession, response);
      try {
        await createPushSubscriptionController(activeSession).disable();
        setPushSubscriptionState(
          response.subscriptionState === "unavailable" ? "unavailable" : "not_enabled",
        );
        setSystemNotificationState(
          response.subscriptionState === "unavailable" ? "unavailable" : "not_enabled",
        );
      } catch (error) {
        setNotificationError(notificationActionError(error));
        setSystemNotificationState("needs_repair");
      }
    } catch (error) {
      setNotificationError(notificationActionError(error));
    } finally {
      setNotificationBusy(false);
    }
  }

  async function handleEnableNotificationOnboarding() {
    const activeSession = deviceSession;
    if (!activeSession || notificationBusy) {
      return;
    }
    setNotificationBusy(true);
    setNotificationError("");
    try {
      await alertPlayerRef.current?.unlock();
    } catch {
      setSoundBlocked(true);
    }
    try {
      if (browserNotificationCapabilities.fixedHttps) {
        await createPushSubscriptionController(activeSession).enable(
          browserNotificationCapabilities,
        );
      }
      const response = await putNotificationSettings(activeSession, {
        ...DEFAULT_NOTIFICATION_SETTINGS,
        enabled: true,
      });
      await applyNotificationResponse(activeSession, response);
      dismissNotificationOnboarding(activeSession.deviceId);
      setShowNotificationOnboarding(false);
      try {
        await sendTestAlert(activeSession);
      } catch {
        setNotificationError("Alerts were enabled, but the test alert could not be sent");
      }
    } catch (error) {
      setNotificationError(notificationActionError(error));
    } finally {
      setNotificationBusy(false);
    }
  }

  function handleDismissNotificationOnboarding() {
    if (deviceSession) {
      dismissNotificationOnboarding(deviceSession.deviceId);
    }
    setShowNotificationOnboarding(false);
  }

  return (
    <main className="app-shell" aria-label="Codex mobile workbench">
      {notificationDeepLinkError ? (
        <p className="notification-deep-link-error" role="alert">
          {notificationDeepLinkError}
        </p>
      ) : null}
      <div className="app-view workbench-app-view" hidden={view !== "workbench"}>
      <ConnectionBar
        bridgeVersion={bridgeVersion}
        connection={connection}
        newSessionDisabled={!canCreateSession || sending}
        pendingApprovalCount={pendingCount}
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
              collapsedProjectIds={collapsedProjectIds}
              onTogglePinned={handleTogglePinnedThread}
              onToggleProject={handleToggleProject}
              pinnedThreadIds={pinnedThreadIds}
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
        collapsedProjectIds={collapsedProjectIds}
        isOpen={isSessionDrawerOpen}
        newSessionDisabled={!canCreateSession || sending}
        onCreateSession={handleOpenNewSession}
        sessions={sessions}
        selectedThreadId={selectedSession?.threadId ?? ""}
        onClose={handleCloseSessionDrawer}
        onOpenSettings={handleOpenSettings}
        onSelect={handleSelectSession}
        onTogglePinned={handleTogglePinnedThread}
        onToggleProject={handleToggleProject}
        pinnedThreadIds={pinnedThreadIds}
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
        disabled={!canSend || sending}
        onAttachFiles={handleAttachDraftImages}
        onDraftChange={setDraft}
        onRemoveAttachment={handleRemoveDraftImage}
        onSubmit={handleSubmit}
      />
      </div>

      <div className="settings-app-view" hidden={view !== "settings"}>
        <NotificationSettingsPage
          busy={notificationBusy}
          browserCapabilities={browserNotificationCapabilities}
          capabilities={notificationCapabilities}
          error={notificationError}
          onBack={() => setView("workbench")}
          onChange={(settings) => void handleSaveNotificationSettings(settings)}
          onDisableAlerts={() => void handleDisableAlerts()}
          onEnableSystemNotifications={() => void handleEnableSystemNotifications()}
          onPreview={(kind) => void handlePreviewAlert(kind)}
          onRepairSystemNotifications={() => void handleRepairSystemNotifications()}
          onSendTest={() => void handleSendTestAlert()}
          settings={notificationSettings}
          systemNotificationState={systemNotificationState}
        />
      </div>

      {soundBlocked ? (
        <button
          className="sound-unlock-prompt"
          onClick={() => void alertPlayerRef.current?.unlock().then(() => setSoundBlocked(false))}
          type="button"
        >
          Tap to enable sound
        </button>
      ) : null}

      {showNotificationOnboarding ? (
        <NotificationOnboardingSheet
          busy={notificationBusy}
          error={notificationError}
          fixedHttps={notificationCapabilities.fixedHttps}
          isIos={browserNotificationCapabilities.isIos}
          onEnable={() => void handleEnableNotificationOnboarding()}
          onNotNow={handleDismissNotificationOnboarding}
          standalone={browserNotificationCapabilities.standalone}
        />
      ) : null}
    </main>
  );
}

function ConnectionBar({
  bridgeVersion,
  connection,
  newSessionDisabled = false,
  onCreateSession,
  onOpenSessions,
  pendingApprovalCount = 0,
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
  pendingApprovalCount?: number;
  sessionMenuButtonRef?: Ref<HTMLButtonElement>;
  showNewSessionButton?: boolean;
  showSessionMenuButton?: boolean;
  statusText: string;
}) {
  const [isDetailOpen, setIsDetailOpen] = useState(false);
  const detailCloseButtonRef = useRef<HTMLButtonElement | null>(null);
  const detailTriggerRef = useRef<HTMLButtonElement | null>(null);
  const secondaryStatusText = statusText === connection.label ? null : statusText;
  const pendingApprovalText = pendingApprovalCount > 0
    ? `${pendingApprovalCount} pending approval${pendingApprovalCount === 1 ? "" : "s"}`
    : null;
  const railStatusText = [connection.detail, pendingApprovalText ?? secondaryStatusText]
    .filter((value, index, values): value is string => Boolean(value) && values.indexOf(value) === index)
    .join(" · ");

  function closeDetails() {
    setIsDetailOpen(false);
    detailTriggerRef.current?.focus();
  }

  useEffect(() => {
    if (!isDetailOpen) {
      return;
    }

    detailCloseButtonRef.current?.focus();
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        closeDetails();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isDetailOpen]);

  useEffect(() => {
    if (!railStatusText) {
      setIsDetailOpen(false);
    }
  }, [railStatusText]);

  return (
    <>
      <header className="connection-bar" aria-label="Connection status">
        <div className="connection-main-row">
          <div className="connection-actions">
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
          </div>
          <div className="connection-primary">
            <h1>Codex Mobile</h1>
            {bridgeVersion ? <span className="app-version">v{bridgeVersion}</span> : null}
          </div>
          <div className="connection-meta">
            <span className={`meta-chip ${connectionClass(connection.label)}`}>{connection.label}</span>
          </div>
        </div>
        <div className="connection-status-rail" aria-label="Bridge status rail">
          <span className="bridge-identity">
            <span className={`status-dot ${connectionClass(connection.label)}`} aria-hidden="true" />
            <span>LAN bridge</span>
          </span>
          {railStatusText ? (
            <button
              className="connection-status-trigger"
              type="button"
              aria-expanded={isDetailOpen}
              aria-label="Show connection details"
              onClick={() => setIsDetailOpen(true)}
              ref={detailTriggerRef}
            >
              <span className="connection-status-message">{railStatusText}</span>
              <ChevronRight size={14} aria-hidden="true" />
            </button>
          ) : null}
        </div>
      </header>
      {isDetailOpen && railStatusText ? (
        <div className="connection-detail-layer">
          <button
            className="connection-detail-backdrop"
            onClick={closeDetails}
            type="button"
            aria-label="Close connection details"
          />
          <section
            className="connection-detail-sheet"
            role="dialog"
            aria-modal="true"
            aria-labelledby="connection-detail-heading"
          >
            <div className="connection-detail-grabber" aria-hidden="true" />
            <div className="connection-detail-heading">
              <div>
                <p className="eyebrow">LAN bridge</p>
                <h2 id="connection-detail-heading">Connection details</h2>
              </div>
              <button
                className="icon-button"
                onClick={closeDetails}
                ref={detailCloseButtonRef}
                type="button"
                aria-label="Close connection details"
              >
                <X size={16} aria-hidden="true" />
              </button>
            </div>
            <div className="connection-detail-content">
              <p>{railStatusText}</p>
              <span className={`connection-detail-state ${connectionClass(connection.label)}`}>
                <span className={`status-dot ${connectionClass(connection.label)}`} aria-hidden="true" />
                {connection.label}
              </span>
            </div>
          </section>
        </div>
      ) : null}
    </>
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
                <ApprovalDetail detail={approval.detail} title={approval.title} />
                {approval.riskHint ? (
                  <p className="risk-line">
                    <ShieldAlert size={13} aria-hidden="true" />
                    {approval.riskHint}
                  </p>
                ) : null}
              </div>
              <div className="approval-actions" aria-label={`${approval.title} decision`}>
                <button
                  className="approval-action-button danger"
                  disabled={disabled}
                  onClick={() => onDecision(approval, "reject")}
                  type="button"
                  aria-label={`Reject ${approval.title}`}
                >
                  <X size={16} aria-hidden="true" />
                  <span>{pendingDecision === "reject" ? "Rejecting…" : "Reject"}</span>
                </button>
                <button
                  className="approval-action-button success"
                  disabled={disabled}
                  onClick={() => onDecision(approval, "approve")}
                  type="button"
                  aria-label={`Approve ${approval.title}`}
                >
                  <Check size={16} aria-hidden="true" />
                  <span>{pendingDecision === "approve" ? "Allowing…" : "Allow once"}</span>
                </button>
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}

function ApprovalDetail({ detail, title }: { detail: string; title: string }) {
  const detailRef = useRef<HTMLParagraphElement | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [overflowing, setOverflowing] = useState(false);

  useLayoutEffect(() => {
    if (expanded) {
      return;
    }

    const detailElement = detailRef.current;
    if (!detailElement) {
      return;
    }

    const measureOverflow = () => {
      const nextOverflowing = detailElement.scrollHeight > detailElement.clientHeight + 1;
      setOverflowing((current) => (current === nextOverflowing ? current : nextOverflowing));
    };

    measureOverflow();
    window.addEventListener("resize", measureOverflow);
    const resizeObserver =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measureOverflow);
    resizeObserver?.observe(detailElement);

    return () => {
      window.removeEventListener("resize", measureOverflow);
      resizeObserver?.disconnect();
    };
  }, [detail, expanded]);

  const showToggle = expanded || overflowing;
  return (
    <div className="approval-detail-shell">
      <p ref={detailRef} className={`approval-detail${expanded ? " expanded" : ""}`}>
        {detail}
      </p>
      {showToggle ? (
        <button
          className="approval-detail-toggle"
          type="button"
          aria-expanded={expanded}
          aria-label={`${expanded ? "Collapse" : "Expand"} ${title}`}
          onClick={() => setExpanded((current) => !current)}
        >
          {expanded ? "Collapse" : "Expand"}
        </button>
      ) : null}
    </div>
  );
}

function SessionDrawer({
  collapsedProjectIds,
  isOpen,
  newSessionDisabled,
  onClose,
  onCreateSession,
  onOpenSettings,
  onSelect,
  onTogglePinned,
  onToggleProject,
  pinnedThreadIds,
  selectedThreadId,
  sessions,
}: {
  collapsedProjectIds: ReadonlySet<string>;
  isOpen: boolean;
  newSessionDisabled: boolean;
  onClose: () => void;
  onCreateSession: () => void;
  onOpenSettings: () => void;
  onSelect: (threadId: string) => void;
  onTogglePinned: (threadId: string) => void;
  onToggleProject: (projectId: string) => void;
  pinnedThreadIds: ReadonlySet<string>;
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
            <button
              className="icon-button"
              onClick={onOpenSettings}
              title="Settings"
              type="button"
              aria-label="Open settings"
            >
              <SettingsIcon size={16} aria-hidden="true" />
            </button>
            <span>{sessions.length}</span>
            <button ref={closeButtonRef} className="icon-button" onClick={onClose} type="button" aria-label="Close sessions">
              <X size={16} aria-hidden="true" />
            </button>
          </div>
        </div>
        <SessionList
          collapsedProjectIds={collapsedProjectIds}
          onSelect={onSelect}
          onTogglePinned={onTogglePinned}
          onToggleProject={onToggleProject}
          pinnedThreadIds={pinnedThreadIds}
          sessions={sessions}
          selectedThreadId={selectedThreadId}
        />
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
  collapsedProjectIds,
  onSelect,
  onTogglePinned,
  onToggleProject,
  pinnedThreadIds,
  selectedThreadId,
  sessions: sessionItems,
}: {
  collapsedProjectIds: ReadonlySet<string>;
  onSelect: (threadId: string) => void;
  onTogglePinned: (threadId: string) => void;
  onToggleProject: (projectId: string) => void;
  pinnedThreadIds: ReadonlySet<string>;
  selectedThreadId: string;
  sessions: SessionSnapshot[];
}) {
  const headingId = useId();
  const projectGroups = useMemo(
    () => groupSessionsByProject(sessionItems, pinnedThreadIds),
    [pinnedThreadIds, sessionItems],
  );

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
        {projectGroups.map((project) => {
          const collapsed = collapsedProjectIds.has(project.id);
          return (
            <section className="session-project" key={project.id} aria-label={`${project.label} project`}>
              <button
                aria-expanded={!collapsed}
                className="session-project-toggle"
                onClick={() => onToggleProject(project.id)}
                title={project.cwd ?? project.label}
                type="button"
              >
                {collapsed ? <ChevronRight size={14} aria-hidden="true" /> : <ChevronDown size={14} aria-hidden="true" />}
                <strong>{project.label}</strong>
                <span>{project.sessions.length}</span>
              </button>
              {!collapsed ? (
                <div className="session-project-threads" role="list">
                  {project.sessions.map((session) => {
                    const selected = session.threadId === selectedThreadId;
                    const pinned = pinnedThreadIds.has(session.threadId);
                    return (
                      <div
                        className={`session-row${selected ? " selected" : ""}`}
                        key={session.threadId}
                        role="listitem"
                      >
                        <button
                          aria-label={session.title}
                          aria-current={selected ? "true" : undefined}
                          className="session-row-select"
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
                        <button
                          aria-label={`${pinned ? "Unpin" : "Pin"} ${session.title}`}
                          aria-pressed={pinned}
                          className={`session-pin-button${pinned ? " pinned" : ""}`}
                          onClick={() => onTogglePinned(session.threadId)}
                          title={pinned ? "Unpin on this phone" : "Pin on this phone"}
                          type="button"
                        >
                          <Pin size={13} aria-hidden="true" />
                        </button>
                      </div>
                    );
                  })}
                </div>
              ) : null}
            </section>
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
  const displayGroups = useMemo(
    () => groupSessionEventsForDisplay(sessionEvents),
    [sessionEvents],
  );

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
        {displayGroups.map((group) =>
          group.kind === "assistant_turn" ? (
            <AssistantTurn
              assetSession={assetSession}
              events={group.events}
              key={group.key}
              sessionStatus={session.status}
            />
          ) : (
            <EventRow
              assetSession={assetSession}
              event={group.event}
              key={group.key}
              sessionStatus={session.status}
            />
          ),
        )}
      </div>
    </section>
  );
}

function AssistantTurn({
  assetSession,
  events,
  sessionStatus,
}: {
  assetSession: DeviceSession | null;
  events: SessionEvent[];
  sessionStatus: SessionStatus;
}) {
  const latestEvent = events.at(-1);
  if (!latestEvent) {
    return null;
  }

  return (
    <article aria-label="Codex response" className="event-row assistant assistant-turn">
      <span className="event-icon" aria-hidden="true">
        <Bot size={14} />
      </span>
      <div className="event-content assistant-turn-content">
        <div className="event-meta assistant-turn-meta">
          <p className="event-kind">Codex</p>
          <time dateTime={new Date(latestEvent.createdAt).toISOString()}>
            {formatEventTime(latestEvent.createdAt)}
          </time>
        </div>
        <div className="assistant-turn-parts">
          {events.map((event) => (
            <AssistantTurnPart
              assetSession={assetSession}
              event={event}
              key={event.id}
              sessionStatus={sessionStatus}
            />
          ))}
        </div>
      </div>
    </article>
  );
}

function AssistantTurnPart({
  assetSession,
  event,
  sessionStatus,
}: {
  assetSession: DeviceSession | null;
  event: SessionEvent;
  sessionStatus: SessionStatus;
}) {
  if (event.type === "reasoning_summary" || event.type === "reasoning_summary_delta") {
    return <ReasoningBlock event={event} sessionStatus={sessionStatus} />;
  }

  if (event.type === "tool_call" || event.type === "tool_result") {
    return <ToolActivity event={event} />;
  }

  const actor = eventActor(event);
  const attachments = payloadImageAttachments(event.payload);
  const isAnswer = actor === "assistant";

  return (
    <section className={`assistant-turn-part${isAnswer ? " answer" : " progress"}`}>
      {!isAnswer ? (
        <div className="assistant-turn-part-meta">
          <span aria-hidden="true">{eventIcon(event, actor)}</span>
          <span>{eventKindLabel(event, actor)}</span>
          <time dateTime={new Date(event.createdAt).toISOString()}>
            {formatEventTime(event.createdAt)}
          </time>
        </div>
      ) : null}
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
    </section>
  );
}

type ToolActivityStatus = "running" | "completed" | "failed" | "declined";

interface ToolActivityView {
  kind: string;
  status: ToolActivityStatus;
  title: string;
  detail: string | null;
}

function ToolActivity({ event }: { event: SessionEvent }) {
  const activity = toolActivityView(event);

  return (
    <section
      aria-label="Tool activity"
      className={`tool-activity ${activity.status}`}
    >
      <span className="tool-activity-icon" aria-hidden="true">
        {toolActivityIcon(activity.kind, activity.status)}
      </span>
      <span className="tool-activity-copy">
        <span className="tool-activity-title">{activity.title}</span>
        {activity.detail ? <span className="tool-activity-detail">{activity.detail}</span> : null}
      </span>
      <time dateTime={new Date(event.createdAt).toISOString()}>
        {formatEventTime(event.createdAt)}
      </time>
    </section>
  );
}

function toolActivityView(event: SessionEvent): ToolActivityView {
  const payload = sessionEventPayloadObject(event.payload);
  const status = toolActivityStatus(payload?.toolStatus, event.type);
  const title = payloadString(payload?.title) ??
    (status === "running"
      ? "Working"
      : status === "completed"
        ? "Finished work"
        : status === "declined"
          ? "Skipped tool"
          : "Tool failed");
  const explicitDetail = payloadString(payload?.detail);
  const fallbackText = payloadText(event.payload);
  const detail = explicitDetail ??
    (fallbackText && fallbackText !== title && !fallbackText.startsWith(`${title}:`)
      ? fallbackText
      : null);

  return {
    kind: payloadString(payload?.toolKind) ?? "tool",
    status,
    title,
    detail,
  };
}

function toolActivityStatus(
  value: SessionEvent["payload"] | undefined,
  eventType: SessionEvent["type"],
): ToolActivityStatus {
  if (value === "running" || value === "completed" || value === "failed" || value === "declined") {
    return value;
  }
  return eventType === "tool_call" ? "running" : "completed";
}

function toolActivityIcon(kind: string, status: ToolActivityStatus) {
  if (status === "completed") {
    return <Check size={14} />;
  }
  if (status === "failed") {
    return <AlertTriangle size={14} />;
  }
  if (status === "declined") {
    return <X size={14} />;
  }

  switch (kind) {
    case "search":
      return <Search size={14} />;
    case "read":
      return <FileText size={14} />;
    case "list_files":
      return <FolderOpen size={14} />;
    case "file_change":
      return <FilePenLine size={14} />;
    case "web_search":
      return <Globe2 size={14} />;
    case "image":
      return <ImageIcon size={14} />;
    case "subagent":
      return <UsersRound size={14} />;
    case "wait":
      return <Hourglass size={14} />;
    case "build":
      return <Hammer size={14} />;
    case "git":
      return <GitBranch size={14} />;
    case "test":
      return <CircleDot size={14} />;
    case "command":
      return <TerminalSquare size={14} />;
    case "review":
      return <ShieldAlert size={14} />;
    default:
      return <Wrench size={14} />;
  }
}

function sessionEventPayloadObject(
  payload: SessionEvent["payload"],
): Record<string, SessionEvent["payload"]> | null {
  return payload !== null && typeof payload === "object" && !Array.isArray(payload)
    ? (payload as Record<string, SessionEvent["payload"]>)
    : null;
}

function payloadString(value: SessionEvent["payload"] | undefined): string | null {
  return typeof value === "string" && value.trim().length > 0 ? value : null;
}

function EventRow({
  assetSession,
  event,
  sessionStatus,
}: {
  assetSession: DeviceSession | null;
  event: SessionEvent;
  sessionStatus: SessionStatus;
}) {
  const attachments = payloadImageAttachments(event.payload);
  const actor = eventActor(event);

  if (event.type === "reasoning_summary" || event.type === "reasoning_summary_delta") {
    return (
      <article className="event-row system reasoning-event">
        <span className="event-icon" aria-hidden="true">
          <Clock3 size={14} />
        </span>
        <ReasoningBlock event={event} sessionStatus={sessionStatus} />
      </article>
    );
  }

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

function ReasoningBlock({
  event,
  sessionStatus,
}: {
  event: SessionEvent;
  sessionStatus: SessionStatus;
}) {
  const [open, setOpen] = useState(sessionStatus === "running");

  useEffect(() => {
    if (sessionStatus === "running") {
      setOpen(true);
    }
  }, [sessionStatus]);

  return (
    <details
      className="reasoning-block"
      onToggle={(toggleEvent) => setOpen(toggleEvent.currentTarget.open)}
      open={open}
    >
      <summary>
        <span>Thinking</span>
        <time dateTime={new Date(event.createdAt).toISOString()}>{formatEventTime(event.createdAt)}</time>
      </summary>
      <div className="reasoning-body">
        <MessageBody text={payloadText(event.payload)} />
      </div>
    </details>
  );
}

type EventActor = "assistant" | "system" | "user";

function eventActor(event: SessionEvent): EventActor {
  if (
    !matchesMessageEvent(event.type) ||
    !event.payload ||
    typeof event.payload !== "object" ||
    Array.isArray(event.payload)
  ) {
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
  if (event.type === "tool_call") {
    return <TerminalSquare size={14} />;
  }
  if (event.type === "plan" || event.type === "plan_delta") {
    return <Command size={14} />;
  }
  return <Clock3 size={14} />;
}

function eventKindLabel(event: SessionEvent, actor: EventActor) {
  if (actor === "user") {
    return "You";
  }
  if (actor === "assistant") {
    return "Codex";
  }
  if (event.type === "plan" || event.type === "plan_delta") {
    return "Plan";
  }
  return event.type.replace("_", " ");
}

function matchesMessageEvent(type: SessionEvent["type"]): boolean {
  return type === "message" || type === "message_delta";
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
    const baseType = baseEventTypeForDelta(event.type);
    if (baseType && canAppendStreamDelta(events[existingIndex], baseType)) {
      const delta = deltaText(event.payload);
      if (!delta) {
        return events;
      }
      const nextEvents = [...events];
      nextEvents[existingIndex] = streamedEvent(events[existingIndex], baseType, delta, event.createdAt);
      return nextEvents;
    }
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
      const existing = events[transientIndex];
      if (
        (isPendingPayload(event.payload) && isBridgeUserEcho(existing)) ||
        (isBridgeUserEcho(event) && isCanonicalUserMessage(existing))
      ) {
        return events;
      }
      const nextEvents = [...events];
      nextEvents[transientIndex] = event;
      return nextEvents;
    }
  }

  const baseType = baseEventTypeForDelta(event.type);
  if (!baseType) {
    return [...events, event];
  }

  const delta = deltaText(event.payload);
  if (!delta) {
    return events;
  }

  const candidate = events.at(-1);
  if (
    candidate &&
    candidate.threadId === event.threadId &&
    canAppendStreamDelta(candidate, baseType)
  ) {
    const nextEvents = [...events];
    nextEvents[nextEvents.length - 1] = streamedEvent(candidate, baseType, delta, event.createdAt);
    return nextEvents;
  }

  return [
    ...events,
    {
      ...event,
      type: baseType,
      payload: streamingPayload(event.payload, baseType, delta),
    },
  ];
}

type StreamingBaseEventType = "message" | "reasoning_summary" | "plan";

function baseEventTypeForDelta(type: SessionEvent["type"]): StreamingBaseEventType | null {
  switch (type) {
    case "message_delta":
      return "message";
    case "reasoning_summary_delta":
      return "reasoning_summary";
    case "plan_delta":
      return "plan";
    default:
      return null;
  }
}

function eventFamily(type: SessionEvent["type"]): StreamingBaseEventType | null {
  return baseEventTypeForDelta(type) ??
    (type === "message" || type === "reasoning_summary" || type === "plan" ? type : null);
}

function canAppendStreamDelta(event: SessionEvent, type: StreamingBaseEventType): boolean {
  if (type === "message") {
    return isAssistantMessage(event);
  }
  return eventFamily(event.type) === type;
}

function streamingRole(type: StreamingBaseEventType): "assistant" | "reasoning" | "plan" {
  if (type === "reasoning_summary") {
    return "reasoning";
  }
  if (type === "plan") {
    return "plan";
  }
  return "assistant";
}

function streamedEvent(
  current: SessionEvent,
  type: StreamingBaseEventType,
  delta: string,
  createdAt: number,
): SessionEvent {
  return {
    ...current,
    type,
    payload: streamingPayload(
      current.payload,
      type,
      `${payloadText(current.payload)}${delta}`,
    ),
    createdAt,
  };
}

function streamingPayload(
  payload: SessionEvent["payload"],
  type: StreamingBaseEventType,
  text: string,
): SessionEvent["payload"] {
  const metadata = payload !== null && typeof payload === "object" && !Array.isArray(payload)
    ? { ...payload }
    : {};
  delete metadata.role;
  delete metadata.text;
  delete metadata.delta;
  return {
    ...metadata,
    role: streamingRole(type),
    text,
  };
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
    if (incremental.some((snapshotEvent) => shouldReplaceLiveTurnEvent(event, snapshotEvent))) {
      return false;
    }
    if (!isPendingPayload(event.payload)) {
      return true;
    }
    const text = normalizedUserMessageText(event);
    return text === null || !incrementalUserTexts.has(text);
  });

  return sortSessionEvents(mergeSessionEvents([...retained, ...incremental]));
}

function shouldReplaceLiveTurnEvent(current: SessionEvent, snapshot: SessionEvent): boolean {
  if (current.threadId !== snapshot.threadId) {
    return false;
  }
  if (current.id === snapshot.id) {
    return true;
  }

  const currentTurnScope = eventTurnScope(current);
  const snapshotTurnScope = eventTurnScope(snapshot);
  if (
    currentTurnScope === null ||
    snapshotTurnScope === null ||
    currentTurnScope !== snapshotTurnScope
  ) {
    return false;
  }

  if (isUserMessage(current) && isUserMessage(snapshot)) {
    return normalizedUserMessageText(current) === normalizedUserMessageText(snapshot);
  }

  const currentFamily = eventFamily(current.type);
  if (currentFamily === null || currentFamily !== eventFamily(snapshot.type)) {
    return false;
  }
  if (
    currentFamily === "message" &&
    (!isAssistantMessage(current) || !isAssistantMessage(snapshot))
  ) {
    return false;
  }

  const currentText = payloadText(current.payload).trim();
  const snapshotText = payloadText(snapshot.payload).trim();
  return (
    currentText.length === 0 ||
    snapshotText.length === 0 ||
    currentText.startsWith(snapshotText) ||
    snapshotText.startsWith(currentText)
  );
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
  const nextBridgeEcho = isBridgeUserEcho(next);

  if (existingPending) {
    return !nextPending;
  }
  if (!existingBridgeEcho && !nextBridgeEcho) {
    return false;
  }
  return Math.abs(existing.createdAt - next.createdAt) <= USER_MESSAGE_RECONCILIATION_WINDOW_MS;
}

function isCanonicalUserMessage(event: SessionEvent): boolean {
  return isUserMessage(event) && !isPendingPayload(event.payload) && !isBridgeUserEcho(event);
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

function userVisibleSessions(items: SessionSnapshot[]): SessionSnapshot[] {
  return items.filter((session) => !isSubagentSession(session));
}

function preferredInitialSessionId(items: SessionSnapshot[]): string {
  return (
    items.find((session) => !isSubagentSession(session) && hasUsefulSessionMetadata(session))
      ?.threadId ??
    items.find((session) => !isSubagentSession(session))?.threadId ??
    items[0]?.threadId ??
    ""
  );
}

function isSubagentSession(session: SessionSnapshot): boolean {
  return (session as SessionSnapshot & { isSubagent?: boolean }).isSubagent === true;
}

function hasUsefulSessionMetadata(session: SessionSnapshot): boolean {
  return (
    (session.title.trim() !== "" && session.title !== session.threadId) ||
    Boolean(session.cwd?.trim()) ||
    Boolean(session.modelProvider?.trim()) ||
    Boolean(session.preview?.trim())
  );
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
  onAlert: (event: AlertEvent) => void,
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
    case "alert_event":
      onAlert(envelope.payload);
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
  const current = items[index] as SessionSnapshot & { isSubagent?: boolean };
  const incoming = next as SessionSnapshot & { isSubagent?: boolean };
  const isSubagent = current.isSubagent === true
    ? true
    : incoming.isSubagent ?? current.isSubagent;
  const replacement: SessionSnapshot & { isSubagent?: boolean } =
    isSubagent === undefined ? next : { ...next, isSubagent };
  const updated = [...items];
  updated[index] = replacement;
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

function readNotificationThreadIdFromUrl(): string | null {
  const threadId = new URL(window.location.href).searchParams.get("threadId");
  return threadId && threadId.length <= 256 ? threadId : null;
}

function clearNotificationThreadParamFromUrl(): void {
  const url = new URL(window.location.href);
  if (!url.searchParams.has("threadId")) {
    return;
  }
  url.searchParams.delete("threadId");
  window.history.replaceState(window.history.state, "", `${url.pathname}${url.search}${url.hash}`);
}

function createPushSubscriptionController(session: DeviceSession): PushSubscriptionController {
  return new PushSubscriptionController({
    permission: () =>
      typeof Notification === "undefined" ? "default" : Notification.permission,
    requestPermission: () => Notification.requestPermission(),
    getPublicKey: () => getPushPublicKey(session),
    getSubscription: async () =>
      (await navigator.serviceWorker.ready).pushManager.getSubscription(),
    subscribe: async (options) =>
      (await navigator.serviceWorker.ready).pushManager.subscribe(options),
    saveSubscription: ({ origin, subscription }) =>
      savePushSubscription(session, origin, subscription),
    deleteServerSubscription: () => deletePushSubscription(session),
    origin: () => window.location.origin,
  });
}

function handleForegroundAlert(
  alert: AlertEvent,
  player: ForegroundAlertPlayer | null,
  settings: DeviceNotificationSettings,
  setSoundBlocked: Dispatch<SetStateAction<boolean>>,
): void {
  void player?.handle(alert, settings).then((result) => {
    if (result.soundBlocked) {
      setSoundBlocked(true);
    }
  });
}

function notificationActionError(error: unknown): string {
  return error instanceof Error ? error.message : "Unable to update notifications";
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
