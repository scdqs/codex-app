import { ArrowLeft, BellRing, Headphones, Send, Volume2 } from "lucide-react";
import type { AlertKind } from "@codex/bridge-protocol";
import type {
  DeviceNotificationSettings,
  NotificationCapabilities,
} from "./api";
import type { PushCapabilities } from "./capabilities";
import type { SystemNotificationState } from "./push-subscription-controller";

export function NotificationSettingsPage({
  busy,
  browserCapabilities,
  capabilities,
  error,
  onBack,
  onChange,
  onDisableAlerts,
  onEnableSystemNotifications,
  onPreview,
  onRepairSystemNotifications,
  onSendTest,
  settings,
  systemNotificationState,
}: {
  busy: boolean;
  browserCapabilities: PushCapabilities;
  capabilities: NotificationCapabilities;
  error?: string;
  onBack: () => void;
  onChange: (settings: DeviceNotificationSettings) => void;
  onDisableAlerts: () => void;
  onEnableSystemNotifications: () => void;
  onPreview: (kind: AlertKind) => void;
  onRepairSystemNotifications: () => void;
  onSendTest: () => void;
  settings: DeviceNotificationSettings;
  systemNotificationState: SystemNotificationState;
}) {
  const kinds: Array<{ kind: AlertKind; label: string; key: keyof DeviceNotificationSettings["alertKinds"] }> = [
    { kind: "completed", label: "Completed alerts", key: "completed" },
    { kind: "approval_required", label: "Approval required alerts", key: "approvalRequired" },
    { kind: "input_required", label: "Input required alerts", key: "inputRequired" },
    { kind: "error", label: "Error alerts", key: "error" },
  ];

  return (
    <section className="notification-settings-page" aria-label="Settings">
      <header className="settings-header">
        <button className="icon-button" onClick={onBack} type="button" aria-label="Back to workbench">
          <ArrowLeft size={17} aria-hidden="true" />
        </button>
        <div>
          <p className="eyebrow">Mobile preferences</p>
          <h1>Settings</h1>
        </div>
      </header>

      {error ? <p className="settings-error" role="alert">{error}</p> : null}

      <SettingsSection title="Notifications">
        <SwitchRow
          checked={settings.enabled}
          disabled={busy}
          label="Task alerts"
          onChange={(enabled) => onChange({ ...settings, enabled })}
        />
        <div className={`settings-system-status status-${systemNotificationState}`}>
          <div>
            <span className="settings-system-label">System notifications</span>
            <strong>{systemNotificationCopy(systemNotificationState).title}</strong>
          </div>
          <p>{systemNotificationCopy(systemNotificationState).detail}</p>
          {browserCapabilities.fixedHttps && browserCapabilities.isIos && !browserCapabilities.standalone ? (
            <p className="settings-install-help">
              Add this app to the Home Screen, then reopen it there before enabling system notifications.
            </p>
          ) : null}
          <div className="settings-system-actions">
            {systemNotificationState === "not_enabled" ? (
              <button disabled={busy} onClick={onEnableSystemNotifications} type="button">
                Enable system notifications
              </button>
            ) : null}
            {systemNotificationState === "needs_repair" ? (
              <button disabled={busy} onClick={onRepairSystemNotifications} type="button">
                Repair notifications
              </button>
            ) : null}
            {systemNotificationState === "active" ||
            systemNotificationState === "needs_repair" ||
            systemNotificationState === "blocked" ||
            settings.enabled ? (
              <button disabled={busy} onClick={onDisableAlerts} type="button">
                Disable alerts
              </button>
            ) : null}
          </div>
        </div>
      </SettingsSection>

      <SettingsSection title="Alert types">
        {kinds.map(({ kind, key, label }) => (
          <div className="settings-alert-row" key={kind}>
            <BellRing size={16} aria-hidden="true" />
            <SwitchRow
              checked={settings.alertKinds[key]}
              disabled={busy}
              label={label}
              onChange={(checked) =>
                onChange({
                  ...settings,
                  alertKinds: { ...settings.alertKinds, [key]: checked },
                })
              }
            />
            <button
              className="settings-preview-button"
              onClick={() => onPreview(kind)}
              title={`Preview ${kind.replaceAll("_", " ")} sound`}
              type="button"
              aria-label={`Preview ${kind.replaceAll("_", " ")} sound`}
            >
              <Headphones size={15} aria-hidden="true" />
            </button>
          </div>
        ))}
      </SettingsSection>

      <SettingsSection title="Delivery">
        <SwitchRow
          checked={settings.soundEnabled}
          disabled={busy || !capabilities.foregroundSound}
          label="Sound"
          onChange={(soundEnabled) => onChange({ ...settings, soundEnabled })}
          icon={<Volume2 size={16} aria-hidden="true" />}
        />
        <SwitchRow
          checked={settings.vibrationEnabled}
          disabled={
            busy ||
            capabilities.vibrationControlledBySystem ||
            browserCapabilities.isIos ||
            !capabilities.foregroundVibration
          }
          label="Vibration"
          onChange={(vibrationEnabled) => onChange({ ...settings, vibrationEnabled })}
        />
        {capabilities.vibrationControlledBySystem || browserCapabilities.isIos ? (
          <p className="settings-help">Vibration is controlled by the iPhone system.</p>
        ) : null}
        <p className="settings-help">
          Foreground alerts use the selected tone. Background alerts use the system notification sound;
          final sound behavior is controlled by the phone.
        </p>
        <button className="settings-test-button" disabled={busy} onClick={onSendTest} type="button">
          <Send size={15} aria-hidden="true" />
          Send test alert
        </button>
      </SettingsSection>

      <SettingsSection title="Connection">
        <p className="settings-connection-title">
          {capabilities.fixedHttps ? "Fixed HTTPS" : "Temporary or local connection"}
        </p>
        <p className="settings-help">
          {capabilities.fixedHttps
            ? "This fixed address supports lock-screen alerts after system notifications are enabled."
            : "Foreground only. Lock-screen alerts require a fixed HTTPS address configured on the Mac."}
        </p>
      </SettingsSection>
    </section>
  );
}

function systemNotificationCopy(state: SystemNotificationState): { title: string; detail: string } {
  return {
    active: {
      title: "Active",
      detail: "Background and lock-screen task alerts are registered for this device.",
    },
    not_enabled: {
      title: "Not enabled",
      detail: "Enable system notifications to receive alerts while this app is in the background.",
    },
    blocked: {
      title: "Blocked",
      detail: "Notifications are blocked. Allow them in the browser or phone settings, then repair.",
    },
    needs_repair: {
      title: "Needs repair",
      detail: "The browser and Bridge notification records no longer match.",
    },
    unavailable: {
      title: "Unavailable",
      detail: "This connection or browser only supports alerts while the page is open.",
    },
  }[state];
}

function SettingsSection({ children, title }: { children: React.ReactNode; title: string }) {
  return (
    <section className="settings-section">
      <h2>{title}</h2>
      <div className="settings-section-body">{children}</div>
    </section>
  );
}

function SwitchRow({
  checked,
  disabled,
  icon,
  label,
  onChange,
}: {
  checked: boolean;
  disabled: boolean;
  icon?: React.ReactNode;
  label: string;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="settings-switch-row">
      <span>{icon}{label}</span>
      <input
        aria-label={label}
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
        role="switch"
        type="checkbox"
      />
    </label>
  );
}
