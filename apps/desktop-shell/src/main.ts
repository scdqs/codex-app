import { invoke } from "@tauri-apps/api/core";
import QRCode from "qrcode";
import {
  escapeHtml,
  nextWizardState,
  renderRemoteAccessPanel,
  type RemoteAccessPreferences,
  type RemoteAccessStatus,
  type RemoteModeSelection,
  type RemoteWizardState,
} from "./remote-access";
import "./styles.css";

type BridgeSnapshot = {
  status: string;
  pid: number | null;
  port: number | null;
  healthUrl: string | null;
  detail: string | null;
};

type CodexOutcome = {
  status: string;
  debugPort: number;
  appPath: string | null;
  detail: string | null;
  instructions: string[];
};

type TunnelSnapshot = {
  status: string;
  publicUrl: string | null;
  localUrl: string | null;
  detail: string | null;
};

type ShellStatus = {
  appVersion: string;
  bridge: BridgeSnapshot;
  tunnel: TunnelSnapshot;
  remoteAccess: RemoteAccessStatus;
  lastPairingLink: string | null;
};

type Device = {
  deviceId: string;
  displayName: string;
  pairedOrigin: string | null;
  createdAt: number;
  lastSeenAt: number;
};

type Diagnostics = {
  status: string;
  connectionState: string;
  detail?: string;
};

type DiagnosticsBundle = Record<string, unknown>;

const appElement = document.querySelector<HTMLElement>("#app");

if (!appElement) {
  throw new Error("missing app root");
}

const app = appElement;

let status: ShellStatus | null = null;
let codexOutcome: CodexOutcome | null = null;
let devices: Device[] = [];
let diagnostics: Diagnostics | null = null;
let pairingQrLink: string | null = null;
let pairingQrDataUrl = "";
let pairingQrError = "";
let busy = false;
let notice = "";
let errorText = "";
let remoteNotice = "";
let remoteError = "";
let remotePreferences: RemoteAccessPreferences | null = null;
let remoteModeSelection: RemoteModeSelection = "named";
let remoteModeInitialized = false;
let remoteWizardInitialized = false;
let remoteWizard: RemoteWizardState = {
  step: 1,
  hostname: "",
  localPort: 57324,
  tokenStored: false,
  tokenDraft: "",
};

function bridgeRunning() {
  return status?.bridge.status === "ready" || status?.bridge.status === "degraded";
}

function setBusy(nextBusy: boolean) {
  busy = nextBusy;
  render();
}

async function runAction(label: string, action: () => Promise<void>) {
  setBusy(true);
  notice = "";
  errorText = "";
  try {
    await action();
    notice = label;
  } catch (error) {
    errorText = error instanceof Error ? error.message : String(error);
  } finally {
    setBusy(false);
    await refresh(false);
  }
}

async function runRemoteAction(label: string, action: () => Promise<void>) {
  setBusy(true);
  remoteNotice = "";
  remoteError = "";
  try {
    await action();
    remoteNotice = label;
  } catch (error) {
    remoteError = error instanceof Error ? error.message : String(error);
  } finally {
    setBusy(false);
    await refresh(false);
  }
}

async function copyText(text: string) {
  await invoke("copy_text", { text });
}

function syncRemotePreferences(
  preferences: RemoteAccessPreferences,
  options: { resetForm?: boolean } = {},
) {
  remotePreferences = preferences;
  const shouldReset = options.resetForm || !remoteWizardInitialized;
  if (shouldReset) {
    remoteWizard = {
      step: preferences.namedProfile ? 3 : 1,
      hostname: preferences.namedProfile?.hostname ?? "",
      localPort: preferences.namedProfile?.localPort ?? 57324,
      tokenStored: preferences.tokenStored,
      tokenDraft: "",
    };
    remoteWizardInitialized = true;
    return;
  }
  remoteWizard = {
    ...remoteWizard,
    tokenStored: preferences.tokenStored,
  };
}

function syncRemoteMode(remoteAccess: RemoteAccessStatus) {
  if (remoteModeInitialized) {
    return;
  }
  remoteModeSelection = remoteAccess.mode === "quick" ? "quick" : "named";
  remoteModeInitialized = true;
}

async function refresh(showErrors = true) {
  try {
    status = await invoke<ShellStatus>("get_app_status");
    syncRemoteMode(status.remoteAccess);
    try {
      const preferences = await invoke<RemoteAccessPreferences>(
        "get_remote_access_preferences",
      );
      syncRemotePreferences(preferences);
    } catch (error) {
      if (showErrors) {
        remoteError = error instanceof Error ? error.message : String(error);
      }
    }
    if (bridgeRunning()) {
      devices = await invoke<Device[]>("list_devices");
      diagnostics = await invoke<Diagnostics>("get_control_diagnostics");
    } else {
      devices = [];
      diagnostics = null;
    }
  } catch (error) {
    if (showErrors) {
      errorText = error instanceof Error ? error.message : String(error);
    }
  }
  render();
}

async function refreshShellStatus() {
  try {
    status = await invoke<ShellStatus>("get_app_status");
    syncRemoteMode(status.remoteAccess);
  } catch {
    return;
  }
  const activeElement = document.activeElement;
  const editingRemoteForm =
    activeElement instanceof HTMLInputElement &&
    Boolean(activeElement.closest(".remote-connect-form"));
  if (
    remoteWizard.step === 2 &&
    remoteModeSelection === "named" &&
    (editingRemoteForm || remoteWizard.tokenDraft.length > 0)
  ) {
    return;
  }
  render();
}

function stoppedRemoteAccess(): RemoteAccessStatus {
  return {
    mode: "none",
    namedProfile: remotePreferences?.namedProfile ?? null,
    named: {
      status: "stopped",
      pid: null,
      localUrl: null,
      publicUrl: null,
      retryAttempt: 0,
      failureKind: null,
      detail: null,
    },
    quick: {
      status: "stopped",
      publicUrl: null,
      localUrl: null,
      detail: null,
    },
    fixedOriginReady: false,
  };
}

function render() {
  syncPairingQr();
  const bridge = status?.bridge;
  app.innerHTML = `
    <section class="shell">
      <header class="topbar">
        <div>
          <p class="eyebrow">Codex Mobile Bridge <span class="app-version">v${escapeHtml(status?.appVersion ?? "-")}</span></p>
          <h1>Mac 控制台</h1>
        </div>
        <div class="top-actions">
          <span class="status-pill ${bridge?.status ?? "stopped"}">${bridge?.status ?? "stopped"}</span>
          <button data-action="refresh" ${busy ? "disabled" : ""}>刷新</button>
        </div>
      </header>

      ${notice ? `<p class="notice">${escapeHtml(notice)}</p>` : ""}
      ${errorText ? `<p class="error">${escapeHtml(errorText)}</p>` : ""}

      <section class="grid">
        <article class="panel">
          <div class="panel-title">
            <h2>ChatGPT / Codex Desktop</h2>
            <button data-action="attach-codex" ${busy ? "disabled" : ""}>检测/启动</button>
          </div>
          ${renderCodexOutcome()}
        </article>

        <article class="panel">
          <div class="panel-title">
            <h2>Bridge Service</h2>
            <div class="button-row">
              <button data-action="start-bridge" ${busy || bridgeRunning() ? "disabled" : ""}>启动</button>
              <button data-action="stop-bridge" ${busy || !bridgeRunning() ? "disabled" : ""}>停止</button>
            </div>
          </div>
          ${renderBridge(bridge)}
        </article>

        <article class="panel wide">
          <div class="panel-title">
            <h2>手机配对</h2>
            <button data-action="pairing-link" ${busy || !bridgeRunning() ? "disabled" : ""}>生成新链接</button>
          </div>
          ${renderPairingLink()}
        </article>

        ${renderRemoteAccessPanel({
          selectedMode: remoteModeSelection,
          wizard: remoteWizard,
          bridgeStatus: bridge?.status ?? "stopped",
          busy,
          remoteAccess: status?.remoteAccess ?? stoppedRemoteAccess(),
          notice: remoteNotice,
          error: remoteError,
        })}

        <article class="panel wide">
          <div class="panel-title">
            <h2>已配对设备</h2>
            <button data-action="load-devices" ${busy || !bridgeRunning() ? "disabled" : ""}>刷新设备</button>
          </div>
          ${renderDevices()}
        </article>

        <article class="panel wide">
          <div class="panel-title">
            <h2>诊断</h2>
            <div class="button-row">
              <button data-action="load-diagnostics" ${busy || !bridgeRunning() ? "disabled" : ""}>刷新诊断</button>
              <button data-action="copy-diagnostics" ${busy ? "disabled" : ""}>复制诊断</button>
            </div>
          </div>
          ${renderDiagnostics()}
        </article>
      </section>
    </section>
  `;

  bindActions();
}

function renderCodexOutcome() {
  if (!codexOutcome) {
    return `<p class="muted">点击检测后，应用会附着已开启 debug port 的 ChatGPT/Codex，或在安全情况下启动对应桌面应用。</p>`;
  }
  return `
    <dl class="facts">
      <div><dt>状态</dt><dd>${escapeHtml(codexOutcome.status)}</dd></div>
      <div><dt>Debug Port</dt><dd>${codexOutcome.debugPort}</dd></div>
      <div><dt>App</dt><dd>${escapeHtml(codexOutcome.appPath ?? "未找到")}</dd></div>
    </dl>
    ${codexOutcome.detail ? `<p class="muted">${escapeHtml(codexOutcome.detail)}</p>` : ""}
    ${codexOutcome.instructions.length ? `<ul>${codexOutcome.instructions.map((item) => `<li>${escapeHtml(item)}</li>`).join("")}</ul>` : ""}
  `;
}

function renderBridge(bridge?: BridgeSnapshot) {
  if (!bridge) {
    return `<p class="muted">Bridge 尚未初始化。</p>`;
  }
  return `
    <dl class="facts">
      <div><dt>状态</dt><dd>${escapeHtml(bridge.status)}</dd></div>
      <div><dt>PID</dt><dd>${bridge.pid ?? "无"}</dd></div>
      <div><dt>端口</dt><dd>${bridge.port ?? "无"}</dd></div>
      <div><dt>Health</dt><dd>${bridge.healthUrl ? link(bridge.healthUrl) : "无"}</dd></div>
    </dl>
    ${bridge.detail ? `<p class="muted">${escapeHtml(bridge.detail)}</p>` : ""}
  `;
}

function renderPairingLink() {
  const pairingLink = status?.lastPairingLink;
  if (!pairingLink) {
    return `<p class="muted">启动 Bridge 后生成手机配对链接。链接为一次性入口。</p>`;
  }
  return `
    <div class="pairing-card">
      <div class="qr-frame">
        ${
          pairingQrDataUrl
            ? `<img class="qr-code" src="${escapeHtml(pairingQrDataUrl)}" alt="手机配对二维码" />`
            : `<span class="qr-placeholder">${pairingQrError ? "二维码生成失败" : "生成中"}</span>`
        }
      </div>
      <div class="pairing-details">
        <p class="link-box">${escapeHtml(pairingLink)}</p>
        <button data-action="copy-pairing">复制链接</button>
      </div>
    </div>
  `;
}

function syncPairingQr() {
  const pairingLink = status?.lastPairingLink ?? null;
  if (pairingLink === pairingQrLink) {
    return;
  }

  pairingQrLink = pairingLink;
  pairingQrDataUrl = "";
  pairingQrError = "";

  if (!pairingLink) {
    return;
  }

  QRCode.toDataURL(pairingLink, {
    errorCorrectionLevel: "M",
    margin: 1,
    width: 192,
    color: {
      dark: "#172026",
      light: "#ffffff",
    },
  })
    .then((dataUrl) => {
      if (pairingQrLink !== pairingLink) {
        return;
      }
      pairingQrDataUrl = dataUrl;
      render();
    })
    .catch((error: unknown) => {
      if (pairingQrLink !== pairingLink) {
        return;
      }
      pairingQrError = error instanceof Error ? error.message : String(error);
      render();
    });
}

function renderDevices() {
  if (!bridgeRunning()) {
    return `<p class="muted">Bridge 启动后才能读取设备。</p>`;
  }
  if (!devices.length) {
    return `<p class="muted">暂无已配对设备。</p>`;
  }
  const currentFixedOrigin = fixedRemoteOrigin();
  return `
    <div class="device-list">
      ${devices
        .map(
          (device) => `
          <div class="device-row">
            <div>
              <strong>${escapeHtml(device.displayName)}</strong>
              <span>${escapeHtml(device.deviceId)}</span>
              <span>${escapeHtml(deviceOriginLabel(device, currentFixedOrigin))}</span>
            </div>
            <button data-action="revoke-device" data-device-id="${escapeHtml(device.deviceId)}" ${busy ? "disabled" : ""}>撤销</button>
          </div>
        `,
        )
        .join("")}
    </div>
  `;
}

function fixedRemoteOrigin() {
  const remoteAccess = status?.remoteAccess;
  if (!remoteAccess?.fixedOriginReady || !remoteAccess.namedProfile) {
    return null;
  }
  return new URL(`https://${remoteAccess.namedProfile.hostname}`).origin;
}

function deviceOriginLabel(device: Device, currentFixedOrigin: string | null) {
  const pairedOrigin = device.pairedOrigin ?? "Origin unknown (paired before v0.1.5)";
  return currentFixedOrigin && device.pairedOrigin !== currentFixedOrigin
    ? `${pairedOrigin} · 旧 Origin`
    : pairedOrigin;
}

function renderDiagnostics() {
  if (!diagnostics) {
    return `<p class="muted">Bridge 启动后显示本地诊断详情。</p>`;
  }
  return `
    <dl class="facts">
      <div><dt>状态</dt><dd>${escapeHtml(diagnostics.status)}</dd></div>
      <div><dt>连接</dt><dd>${escapeHtml(diagnostics.connectionState)}</dd></div>
    </dl>
    ${diagnostics.detail ? `<pre>${escapeHtml(diagnostics.detail)}</pre>` : `<p class="muted">暂无详细错误。</p>`}
  `;
}

function bindActions() {
  app.querySelectorAll<HTMLButtonElement>("button[data-action]").forEach((button) => {
    button.addEventListener("click", async () => {
      const action = button.dataset.action;
      const inRemotePanel = Boolean(button.closest(".remote-access-panel"));
      if (action === "refresh") await refresh();
      if (action === "attach-codex") {
        await runAction("ChatGPT/Codex 检测完成", async () => {
          codexOutcome = await invoke<CodexOutcome>("ensure_codex_ready");
        });
      }
      if (action === "start-bridge") await runAction("Bridge 已启动", () => invoke("start_bridge"));
      if (action === "stop-bridge") await runAction("Bridge 已停止", () => invoke("stop_bridge"));
      if (action === "pairing-link") await runAction("已生成新配对链接", () => invoke("create_pairing_link"));
      if (action === "remote-wizard-continue") {
        remoteWizard = nextWizardState(remoteWizard, { type: "continue" });
        render();
      }
      if (action === "remote-wizard-back") {
        remoteWizard = nextWizardState(remoteWizard, { type: "back" });
        render();
      }
      if (action === "save-named-profile") {
        const nextState = nextWizardState(remoteWizard, { type: "continue" });
        if (nextState.error) {
          remoteWizard = nextState;
          render();
        } else {
          await runRemoteAction("固定域名配置已保存", async () => {
            const preferences = await invoke<RemoteAccessPreferences>(
              "save_named_tunnel_profile",
              {
                hostname: remoteWizard.hostname,
                localPort: remoteWizard.localPort,
                token: remoteWizard.tokenDraft.trim() || null,
              },
            );
            syncRemotePreferences(preferences, { resetForm: true });
            remoteModeSelection = "named";
          });
        }
      }
      if (action === "edit-named-tunnel") {
        remoteModeSelection = "named";
        remoteWizard = {
          ...remoteWizard,
          step: 2,
          error: undefined,
        };
        remoteNotice = "";
        remoteError = "";
        render();
      }
      if (action === "start-named-tunnel") {
        remoteModeSelection = "named";
        await runRemoteAction("固定域名验证完成", () => invoke("start_named_tunnel"));
      }
      if (action === "retry-named-tunnel") {
        remoteModeSelection = "named";
        await runRemoteAction("固定域名已重新验证", () => invoke("retry_named_tunnel"));
      }
      if (action === "recheck-named-tunnel-health") {
        await runRemoteAction("固定域名状态已刷新", () =>
          invoke("recheck_named_tunnel_health"),
        );
      }
      if (action === "stop-named-tunnel") {
        await runRemoteAction("固定域名已关闭", () => invoke("stop_named_tunnel"));
      }
      if (action === "start-temporary-tunnel") {
        await runRemoteAction("临时通道已启动", async () => {
          await invoke("start_temporary_tunnel");
          remoteModeSelection = "quick";
        });
      }
      if (action === "rotate-quick-tunnel") {
        await runRemoteAction("临时链接已更换", () => invoke("rotate_quick_tunnel"));
      }
      if (action === "stop-quick-tunnel") {
        await runRemoteAction("临时通道已关闭", () => invoke("stop_quick_tunnel"));
      }
      if (action === "copy-origin-service") {
        await copyText(`http://localhost:${remoteWizard.localPort}`);
        remoteNotice = "Origin Service 已复制";
        remoteError = "";
        render();
      }
      if (action === "copy-remote-url") {
        const remoteAccess = status?.remoteAccess;
        const url =
          remoteModeSelection === "quick"
            ? remoteAccess?.quick.publicUrl
            : remoteAccess?.named.publicUrl;
        if (url) {
          await copyText(url);
          remoteNotice = "远程 URL 已复制";
          remoteError = "";
          render();
        }
      }
      if (action === "load-devices") await refresh();
      if (action === "load-diagnostics") await refresh();
      if (action === "copy-diagnostics") {
        const runner = inRemotePanel ? runRemoteAction : runAction;
        await runner("诊断 JSON 已复制", async () => {
          const bundle = await invoke<DiagnosticsBundle>("get_diagnostics_bundle");
          await copyText(JSON.stringify(bundle, null, 2));
        });
      }
      if (action === "copy-pairing" && status?.lastPairingLink) {
        await copyText(status.lastPairingLink);
        notice = "配对链接已复制";
        render();
      }
      if (action === "revoke-device") {
        const deviceId = button.dataset.deviceId;
        if (deviceId) {
          await runAction("设备已撤销", () => invoke("revoke_device", { deviceId }));
        }
      }
    });
  });

  app.querySelectorAll<HTMLInputElement>("input[data-remote-mode]").forEach((input) => {
    input.addEventListener("change", () => {
      remoteModeSelection = input.value === "quick" ? "quick" : "named";
      remoteNotice = "";
      remoteError = "";
      render();
    });
  });

  app.querySelector<HTMLInputElement>('input[name="publicHostname"]')?.addEventListener(
    "input",
    (event) => {
      remoteWizard = {
        ...remoteWizard,
        hostname: (event.currentTarget as HTMLInputElement).value,
        error: undefined,
      };
    },
  );
  app.querySelector<HTMLInputElement>('input[name="tunnelToken"]')?.addEventListener(
    "input",
    (event) => {
      remoteWizard = {
        ...remoteWizard,
        tokenDraft: (event.currentTarget as HTMLInputElement).value,
        error: undefined,
      };
    },
  );
  app.querySelector<HTMLInputElement>('input[name="localPort"]')?.addEventListener(
    "input",
    (event) => {
      const value = Number((event.currentTarget as HTMLInputElement).value);
      remoteWizard = {
        ...remoteWizard,
        localPort: Number.isFinite(value) ? value : remoteWizard.localPort,
        error: undefined,
      };
    },
  );
}

function link(url: string) {
  return `<a href="${escapeHtml(url)}" target="_blank" rel="noreferrer">${escapeHtml(url)}</a>`;
}

void refresh(false);
window.setInterval(() => void refreshShellStatus(), 5_000);
