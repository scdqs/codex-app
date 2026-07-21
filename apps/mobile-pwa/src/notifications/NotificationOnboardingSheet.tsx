export function NotificationOnboardingSheet({
  busy,
  error,
  fixedHttps,
  isIos,
  onEnable,
  onNotNow,
  standalone,
}: {
  busy: boolean;
  error?: string;
  fixedHttps: boolean;
  isIos: boolean;
  onEnable: () => void;
  onNotNow: () => void;
  standalone: boolean;
}) {
  return (
    <div className="notification-onboarding-layer" role="presentation">
      <section className="notification-onboarding" role="dialog" aria-modal="true" aria-label="Enable task alerts">
        <p className="eyebrow">Stay in the loop</p>
        <h2>Get notified when Codex needs you</h2>
        <p>
          Hear a distinct alert when a task completes, needs approval, needs input, or fails.
          {!fixedHttps
            ? " This connection supports foreground only alerts. Lock-screen alerts require a fixed HTTPS address configured on the Mac."
            : ""}
        </p>
        {fixedHttps && isIos && !standalone ? (
          <p className="notification-install-help">
            On iPhone, tap Share, choose Add to Home Screen, then reopen it from the Home Screen.
          </p>
        ) : null}
        {error ? <p className="settings-error" role="alert">{error}</p> : null}
        <div className="notification-onboarding-actions">
          <button className="primary-action" disabled={busy} onClick={onEnable} type="button">
            {busy ? "Enabling…" : "Enable alerts"}
          </button>
          <button className="secondary-action" disabled={busy} onClick={onNotNow} type="button">
            Not now
          </button>
        </div>
      </section>
    </div>
  );
}
