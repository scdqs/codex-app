export type WizardStep = 1 | 2 | 3;

export type RemoteFailureAction = "retry" | "edit" | "diagnostics" | "start_temporary";

export type RemoteAccessMode = "none" | "quick" | "named" | "named_failed";

export type RemoteModeSelection = "named" | "quick";

export interface RemoteWizardState {
  step: WizardStep;
  hostname: string;
  localPort: number;
  tokenStored: boolean;
  tokenDraft: string;
  error?: string;
}

export interface NamedTunnelProfile {
  hostname: string;
  localPort: number;
}

export interface NamedTunnelSnapshot {
  status: string;
  pid: number | null;
  localUrl: string | null;
  publicUrl: string | null;
  retryAttempt: number;
  failureKind: string | null;
  detail: string | null;
}

export interface QuickTunnelSnapshot {
  status: string;
  publicUrl: string | null;
  localUrl: string | null;
  detail: string | null;
}

export interface RemoteAccessStatus {
  mode: RemoteAccessMode;
  namedProfile: NamedTunnelProfile | null;
  named: NamedTunnelSnapshot;
  quick: QuickTunnelSnapshot;
  fixedOriginReady: boolean;
}

export interface RemoteAccessPreferences {
  namedProfile: NamedTunnelProfile | null;
  tokenStored: boolean;
}

export interface RemoteAccessPanelModel {
  selectedMode: RemoteModeSelection;
  wizard: RemoteWizardState;
  bridgeStatus: string;
  busy: boolean;
  remoteAccess: RemoteAccessStatus;
  notice?: string;
  error?: string;
}

export function nextWizardState(
  state: RemoteWizardState,
  action: { type: "continue" | "back" | "edit" },
): RemoteWizardState {
  if (action.type === "back" || action.type === "edit") {
    return {
      ...state,
      step: Math.max(1, state.step - 1) as WizardStep,
      error: undefined,
    };
  }

  if (
    state.step === 2 &&
    (!state.hostname.trim() || (!state.tokenStored && !state.tokenDraft.trim()))
  ) {
    return {
      ...state,
      error: "Public Hostname and Tunnel Token are required",
    };
  }

  return {
    ...state,
    step: Math.min(3, state.step + 1) as WizardStep,
    error: undefined,
  };
}

export function remoteFailureActions(status: string): RemoteFailureAction[] {
  return status === "failed"
    ? ["retry", "edit", "diagnostics", "start_temporary"]
    : [];
}

export function renderRemoteAccessPanel(model: RemoteAccessPanelModel): string {
  const selectedMode =
    model.remoteAccess.mode === "named_failed" ? "named" : model.selectedMode;
  const activeStatus =
    model.remoteAccess.mode === "quick"
      ? model.remoteAccess.quick.status
      : model.remoteAccess.named.status;

  return `
    <article class="panel wide remote-access-panel">
      <div class="panel-title remote-access-title">
        <div>
          <p class="eyebrow">Remote Access</p>
          <h2>远程访问</h2>
        </div>
        <span class="status-pill ${statusClass(activeStatus)}">${escapeHtml(activeStatus)}</span>
      </div>

      <div class="remote-mode-switch" role="radiogroup" aria-label="远程访问模式">
        <label>
          <input
            type="radio"
            name="remoteMode"
            value="named"
            data-remote-mode="named"
            ${selectedMode === "named" ? "checked" : ""}
          />
          <span>固定域名</span>
        </label>
        <label>
          <input
            type="radio"
            name="remoteMode"
            value="quick"
            data-remote-mode="quick"
            ${selectedMode === "quick" ? "checked" : ""}
          />
          <span>临时通道</span>
        </label>
      </div>

      ${model.notice ? `<p class="remote-notice">${escapeHtml(model.notice)}</p>` : ""}
      ${model.error ? `<p class="remote-error">${escapeHtml(model.error)}</p>` : ""}
      ${
        model.remoteAccess.mode === "quick" && selectedMode === "named"
          ? `<p class="remote-active-mode-warning">当前为临时 URL；锁屏通知已暂停</p>`
          : ""
      }
      ${selectedMode === "quick" ? renderQuickAccess(model) : renderNamedAccess(model)}
    </article>
  `;
}

function renderNamedAccess(model: RemoteAccessPanelModel): string {
  if (model.remoteAccess.mode === "named_failed") {
    return renderNamedFailure(model);
  }

  return `
    ${renderStepper(model.wizard.step)}
    <div class="remote-step-content">
      ${renderWizardStep(model)}
    </div>
  `;
}

function renderStepper(step: WizardStep): string {
  const steps: Array<[WizardStep, string]> = [
    [1, "Create Tunnel"],
    [2, "Connect Bridge"],
    [3, "Verify"],
  ];

  return `
    <ol class="remote-stepper" aria-label="固定域名设置进度">
      ${steps
        .map(
          ([number, label]) => `
            <li class="${number === step ? "active" : number < step ? "complete" : ""}">
              <span>${number}</span>
              <strong>${label}</strong>
            </li>
          `,
        )
        .join("")}
    </ol>
  `;
}

function renderWizardStep(model: RemoteAccessPanelModel): string {
  if (model.wizard.step === 1) {
    return renderCreateTunnelStep(model);
  }
  if (model.wizard.step === 2) {
    return renderConnectBridgeStep(model);
  }
  return renderVerifyStep(model);
}

function renderCreateTunnelStep(model: RemoteAccessPanelModel): string {
  const originService = `http://localhost:${model.wizard.localPort}`;

  return `
    <div class="remote-step-heading">
      <div>
        <h3>Create Tunnel</h3>
        <p class="muted">在 Cloudflare 创建 Named Tunnel，并把 Public Hostname 指向本机 Bridge。</p>
      </div>
      <a href="https://one.dash.cloudflare.com/" target="_blank" rel="noreferrer">Cloudflare Dashboard</a>
    </div>
    <dl class="remote-definition-list">
      <div>
        <dt>Origin Service</dt>
        <dd><code>${escapeHtml(originService)}</code></dd>
      </div>
    </dl>
    <div class="remote-actions">
      <button data-action="copy-origin-service" ${disabled(model.busy)}>复制 Origin</button>
      <button class="primary" data-action="remote-wizard-continue" ${disabled(model.busy)}>继续</button>
    </div>
  `;
}

function renderConnectBridgeStep(model: RemoteAccessPanelModel): string {
  return `
    <div class="remote-step-heading">
      <div>
        <h3>Connect Bridge</h3>
        <p class="muted">保存固定域名和 Tunnel Token。Token 只写入 macOS Keychain。</p>
      </div>
    </div>
    <form class="remote-connect-form" autocomplete="off">
      <div class="remote-form-grid">
        <label>
          <span>Public Hostname</span>
          <input
            type="text"
            name="publicHostname"
            autocomplete="url"
            spellcheck="false"
            value="${escapeHtml(model.wizard.hostname)}"
            placeholder="remote.example.com"
            ${disabled(model.busy)}
          />
        </label>
        <label>
          <span>Tunnel Token</span>
          <input
            type="password"
            name="tunnelToken"
            autocomplete="off"
            value=""
            placeholder="${model.wizard.tokenStored ? "留空以继续使用已保存 Token" : "粘贴 Cloudflare Tunnel Token"}"
            ${disabled(model.busy)}
          />
          ${
            model.wizard.tokenStored
              ? `<small class="keychain-state">已安全保存在 Keychain</small>`
              : ""
          }
        </label>
        <label>
          <span>Local Port</span>
          <input
            type="number"
            name="localPort"
            min="1"
            max="65535"
            step="1"
            value="${model.wizard.localPort}"
            ${disabled(model.busy)}
          />
        </label>
      </div>
      ${
        model.wizard.error
          ? `<p class="remote-inline-error">${escapeHtml(model.wizard.error)}</p>`
          : ""
      }
      <div class="remote-actions">
        <button type="button" data-action="remote-wizard-back" ${disabled(model.busy)}>返回</button>
        <button type="button" class="primary" data-action="save-named-profile" ${disabled(model.busy)}>保存并继续</button>
      </div>
    </form>
  `;
}

function renderVerifyStep(model: RemoteAccessPanelModel): string {
  const named = model.remoteAccess.named;
  const namedActive = model.remoteAccess.mode === "named";
  const publicHealth = model.remoteAccess.fixedOriginReady
    ? "ready"
    : namedActive && named.status === "degraded"
      ? "degraded"
      : "pending";
  const sameBridge = model.remoteAccess.fixedOriginReady ? "ready" : "pending";
  const hostname = model.wizard.hostname || model.remoteAccess.namedProfile?.hostname || "未配置";
  const isRunning = namedActive && named.status !== "stopped" && named.status !== "failed";
  const isDegraded = namedActive && named.status === "degraded";

  return `
    <div class="remote-step-heading">
      <div>
        <h3>Verify</h3>
        <p class="muted">启动固定域名通道并确认本地与公网访问指向同一 Bridge。</p>
      </div>
    </div>
    ${
      isDegraded
        ? `<div class="remote-status-strip degraded">
            <span>固定域名仍在运行，但 Public health 尚未通过。</span>
            <button data-action="recheck-named-tunnel-health" ${disabled(model.busy)}>立即重新检测</button>
          </div>`
        : ""
    }
    <dl class="remote-definition-list remote-verify-summary">
      <div><dt>Public Hostname</dt><dd>${escapeHtml(hostname)}</dd></div>
      ${named.localUrl ? `<div><dt>Local URL</dt><dd>${safeLink(named.localUrl)}</dd></div>` : ""}
      ${named.publicUrl ? `<div><dt>Public URL</dt><dd>${safeLink(named.publicUrl)}</dd></div>` : ""}
    </dl>
    <ul class="remote-status-list">
      ${renderStatusItem("Local Bridge", model.bridgeStatus)}
      ${renderStatusItem("Cloudflare connection", namedActive ? named.status : "pending")}
      ${renderStatusItem("Public health", publicHealth)}
      ${renderStatusItem("Same Bridge instance", sameBridge)}
    </ul>
    ${named.detail ? `<p class="remote-detail">${escapeHtml(named.detail)}</p>` : ""}
    ${model.wizard.error ? `<p class="remote-inline-error">${escapeHtml(model.wizard.error)}</p>` : ""}
    <div class="remote-actions">
      ${
        isDegraded
          ? ""
          : `<button data-action="edit-named-tunnel" ${disabled(model.busy)}>修改配置</button>`
      }
      ${
        named.publicUrl
          ? `<button data-action="copy-remote-url" ${disabled(model.busy)}>复制 URL</button>`
          : ""
      }
      ${
        isRunning
          ? `<button data-action="stop-named-tunnel" ${disabled(model.busy)}>关闭固定域名</button>`
          : `<button class="primary" data-action="start-named-tunnel" ${disabled(
              model.busy || !model.remoteAccess.namedProfile,
            )}>开始验证</button>`
      }
    </div>
  `;
}

function renderNamedFailure(model: RemoteAccessPanelModel): string {
  const named = model.remoteAccess.named;
  const actionNames: Record<RemoteFailureAction, string> = {
    retry: "retry-named-tunnel",
    edit: "edit-named-tunnel",
    diagnostics: "copy-diagnostics",
    start_temporary: "start-temporary-tunnel",
  };
  const actionLabels: Record<RemoteFailureAction, string> = {
    retry: "重试",
    edit: "修改配置",
    diagnostics: "查看诊断",
    start_temporary: "启动临时通道",
  };

  return `
    <div class="remote-failure" role="alert">
      <div class="remote-step-heading">
        <div>
          <h3>固定域名连接失败</h3>
          <p class="muted">已停止自动重试，配置会保留。临时通道只会在你明确选择后启动。</p>
        </div>
      </div>
      <dl class="remote-definition-list">
        <div><dt>状态</dt><dd>${escapeHtml(named.status)}</dd></div>
        ${named.failureKind ? `<div><dt>失败类型</dt><dd>${escapeHtml(named.failureKind)}</dd></div>` : ""}
        <div><dt>重试次数</dt><dd>${named.retryAttempt}</dd></div>
      </dl>
      ${named.detail ? `<p class="remote-detail">${escapeHtml(named.detail)}</p>` : ""}
      ${model.wizard.error ? `<p class="remote-inline-error">${escapeHtml(model.wizard.error)}</p>` : ""}
      <div class="remote-actions remote-failure-actions">
        ${remoteFailureActions(named.status)
          .map(
            (action) =>
              `<button data-action="${actionNames[action]}" ${disabled(model.busy)}>${actionLabels[action]}</button>`,
          )
          .join("")}
      </div>
    </div>
  `;
}

function renderQuickAccess(model: RemoteAccessPanelModel): string {
  const quick = model.remoteAccess.quick;
  const quickRunning =
    model.remoteAccess.mode === "quick" &&
    (quick.status === "ready" || quick.status === "reconnecting");

  return `
    <div class="remote-quick-view">
      <div class="remote-step-heading">
        <div>
          <h3>临时通道</h3>
          <p class="muted">Quick Tunnel 会生成可更换的临时 URL，适合固定域名不可用时手动应急。</p>
        </div>
      </div>
      ${
        quickRunning
          ? `<div class="remote-status-strip warning">当前为临时 URL；锁屏通知已暂停</div>`
          : ""
      }
      <dl class="remote-definition-list">
        <div><dt>状态</dt><dd>${escapeHtml(quick.status)}</dd></div>
        ${quick.publicUrl ? `<div><dt>Public URL</dt><dd>${safeLink(quick.publicUrl)}</dd></div>` : ""}
        ${quick.localUrl ? `<div><dt>Local URL</dt><dd>${safeLink(quick.localUrl)}</dd></div>` : ""}
      </dl>
      ${quick.detail ? `<p class="remote-detail">${escapeHtml(quick.detail)}</p>` : ""}
      <div class="remote-actions">
        ${
          quickRunning && quick.publicUrl
            ? `<button data-action="copy-remote-url" ${disabled(model.busy)}>复制 URL</button>`
            : ""
        }
        ${
          quickRunning
            ? `<button data-action="rotate-quick-tunnel" ${disabled(model.busy)}>换链接</button>
              <button data-action="stop-quick-tunnel" ${disabled(model.busy)}>关闭</button>`
            : `<button class="primary" data-action="start-temporary-tunnel" ${disabled(
                model.busy || !isBridgeAvailable(model.bridgeStatus),
              )}>启动临时通道</button>`
        }
      </div>
    </div>
  `;
}

function renderStatusItem(label: string, value: string): string {
  return `
    <li>
      <span>${label}</span>
      <strong class="${statusClass(value)}">${escapeHtml(value)}</strong>
    </li>
  `;
}

function safeLink(url: string): string {
  const escapedUrl = escapeHtml(url);
  return `<a href="${escapedUrl}" target="_blank" rel="noreferrer">${escapedUrl}</a>`;
}

function statusClass(status: string): string {
  switch (status) {
    case "ready":
      return "ready";
    case "degraded":
    case "reconnecting":
    case "starting":
    case "retrying":
      return "degraded";
    case "failed":
      return "failed";
    default:
      return "stopped";
  }
}

function isBridgeAvailable(status: string): boolean {
  return status === "ready" || status === "degraded";
}

function disabled(value: boolean): string {
  return value ? "disabled" : "";
}

export function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => {
    const entities: Record<string, string> = {
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#39;",
    };
    return entities[character] ?? character;
  });
}
