import { invoke } from "@tauri-apps/api/core";
import QRCode from "qrcode";
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
  remoteAccess?: {
    fixedOriginReady: boolean;
    namedProfile: {
      hostname: string;
      localPort: number;
    } | null;
  };
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

function bridgeRunning() {
  return status?.bridge.status === "ready" || status?.bridge.status === "degraded";
}

function tunnelRunning() {
  return status?.tunnel.status === "ready" || status?.tunnel.status === "reconnecting";
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

async function copyText(text: string) {
  await invoke("copy_text", { text });
}

async function refresh(showErrors = true) {
  try {
    status = await invoke<ShellStatus>("get_app_status");
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
  } catch {
    return;
  }
  render();
}

function render() {
  syncPairingQr();
  const bridge = status?.bridge;
  const tunnel = status?.tunnel;
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

        <article class="panel">
          <div class="panel-title">
            <h2>手机配对</h2>
            <button data-action="pairing-link" ${busy || !bridgeRunning() ? "disabled" : ""}>生成新链接</button>
          </div>
          ${renderPairingLink()}
        </article>

        <article class="panel">
          <div class="panel-title">
            <h2>远程链接 Beta</h2>
            <div class="button-row">
              <button data-action="start-tunnel" ${busy || !bridgeRunning() || tunnelRunning() ? "disabled" : ""}>开启</button>
              <button data-action="rotate-tunnel" ${busy || !tunnelRunning() ? "disabled" : ""}>换链接</button>
              <button data-action="stop-tunnel" ${busy || !tunnelRunning() ? "disabled" : ""}>关闭</button>
            </div>
          </div>
          ${renderTunnel(tunnel)}
        </article>

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

function renderTunnel(tunnel?: TunnelSnapshot) {
  if (!tunnel) {
    return `<p class="muted">远程链接默认关闭。</p>`;
  }
  return `
    <dl class="facts">
      <div><dt>状态</dt><dd>${escapeHtml(tunnel.status)}</dd></div>
      <div><dt>Public URL</dt><dd>${tunnel.publicUrl ? link(tunnel.publicUrl) : "无"}</dd></div>
      <div><dt>Local URL</dt><dd>${tunnel.localUrl ? link(tunnel.localUrl) : "无"}</dd></div>
    </dl>
    <p class="muted">Quick Tunnel 是临时 Beta 能力，链接旋转后需要重新配对。</p>
    ${tunnel.detail ? `<p class="muted">${escapeHtml(tunnel.detail)}</p>` : ""}
  `;
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
      if (action === "refresh") await refresh();
      if (action === "attach-codex") {
        await runAction("ChatGPT/Codex 检测完成", async () => {
          codexOutcome = await invoke<CodexOutcome>("ensure_codex_ready");
        });
      }
      if (action === "start-bridge") await runAction("Bridge 已启动", () => invoke("start_bridge"));
      if (action === "stop-bridge") await runAction("Bridge 已停止", () => invoke("stop_bridge"));
      if (action === "pairing-link") await runAction("已生成新配对链接", () => invoke("create_pairing_link"));
      if (action === "start-tunnel") await runAction("远程链接已开启", () => invoke("start_quick_tunnel"));
      if (action === "rotate-tunnel") await runAction("远程链接已更换", () => invoke("rotate_quick_tunnel"));
      if (action === "stop-tunnel") await runAction("远程链接已关闭", () => invoke("stop_quick_tunnel"));
      if (action === "load-devices") await refresh();
      if (action === "load-diagnostics") await refresh();
      if (action === "copy-diagnostics") {
        await runAction("诊断 JSON 已复制", async () => {
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
}

function link(url: string) {
  return `<a href="${escapeHtml(url)}" target="_blank" rel="noreferrer">${escapeHtml(url)}</a>`;
}

function escapeHtml(value: string) {
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

void refresh(false);
window.setInterval(() => void refreshShellStatus(), 5_000);
