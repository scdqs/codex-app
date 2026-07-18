// @vitest-environment jsdom

import { describe, expect, it } from "vitest";
import {
  nextWizardState,
  remoteFailureActions,
  renderRemoteAccessPanel,
  type RemoteAccessPanelModel,
  type RemoteWizardState,
} from "./remote-access";

function wizard(overrides: Partial<RemoteWizardState> = {}): RemoteWizardState {
  return {
    step: 1,
    hostname: "remote.example.com",
    localPort: 43123,
    tokenStored: false,
    tokenDraft: "token-draft",
    ...overrides,
  };
}

function model(overrides: Partial<RemoteAccessPanelModel> = {}): RemoteAccessPanelModel {
  return {
    selectedMode: "named",
    wizard: wizard(),
    bridgeStatus: "ready",
    busy: false,
    remoteAccess: {
      mode: "none",
      namedProfile: null,
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
    },
    ...overrides,
  };
}

function renderDom(panelModel: RemoteAccessPanelModel) {
  document.body.innerHTML = renderRemoteAccessPanel(panelModel);
  return document.body;
}

describe("nextWizardState", () => {
  it("keeps step 2 and requires hostname plus a first-use token", () => {
    const missingHostname = nextWizardState(
      wizard({ step: 2, hostname: "", tokenStored: true, tokenDraft: "" }),
      { type: "continue" },
    );
    const missingFirstToken = nextWizardState(
      wizard({ step: 2, hostname: "remote.example.com", tokenStored: false, tokenDraft: "" }),
      { type: "continue" },
    );

    expect(missingHostname).toMatchObject({
      step: 2,
      error: "Public Hostname and Tunnel Token are required",
    });
    expect(missingFirstToken).toMatchObject({
      step: 2,
      error: "Public Hostname and Tunnel Token are required",
    });
  });

  it("honors back and edit boundaries and continues through step 3", () => {
    expect(nextWizardState(wizard({ step: 1, error: "old" }), { type: "back" })).toMatchObject({
      step: 1,
      error: undefined,
    });
    expect(nextWizardState(wizard({ step: 2 }), { type: "back" }).step).toBe(1);
    expect(nextWizardState(wizard({ step: 3 }), { type: "edit" }).step).toBe(2);
    expect(nextWizardState(wizard({ step: 2 }), { type: "continue" }).step).toBe(3);
    expect(nextWizardState(wizard({ step: 3 }), { type: "continue" }).step).toBe(3);
  });
});

describe("remoteFailureActions", () => {
  it("returns the four manual recovery choices only for failed", () => {
    expect(remoteFailureActions("failed")).toEqual([
      "retry",
      "edit",
      "diagnostics",
      "start_temporary",
    ]);
    expect(remoteFailureActions("degraded")).toEqual([]);
    expect(remoteFailureActions("ready")).toEqual([]);
  });
});

describe("renderRemoteAccessPanel", () => {
  it("escapes hostname, URLs, detail, error, status, and notice text", () => {
    const html = renderRemoteAccessPanel(
      model({
        notice: '<mark data-secret="notice">notice</mark>',
        error: '<strong data-secret="error">error</strong>',
        wizard: wizard({
          step: 3,
          hostname: '<img src=x onerror="hostname">',
          error: '<em data-secret="wizard">wizard error</em>',
        }),
        remoteAccess: {
          mode: "named",
          namedProfile: {
            hostname: '<img src=x onerror="profile">',
            localPort: 43123,
          },
          named: {
            status: '<ready data-secret="status">',
            pid: 12,
            localUrl: 'http://localhost:43123/?q=<local data-secret="url">',
            publicUrl: 'https://remote.example.com/?q=<public data-secret="url">',
            retryAttempt: 1,
            failureKind: "health_check_failed",
            detail: '<script data-secret="detail">detail</script>',
          },
          quick: {
            status: "stopped",
            publicUrl: null,
            localUrl: null,
            detail: null,
          },
          fixedOriginReady: false,
        },
      }),
    );

    expect(html).toContain("&lt;img src=x onerror=&quot;hostname&quot;&gt;");
    expect(html).toContain("&lt;ready data-secret=&quot;status&quot;&gt;");
    expect(html).toContain("&lt;script data-secret=&quot;detail&quot;&gt;detail&lt;/script&gt;");
    expect(html).toContain("&lt;strong data-secret=&quot;error&quot;&gt;error&lt;/strong&gt;");
    expect(html).toContain("&lt;mark data-secret=&quot;notice&quot;&gt;notice&lt;/mark&gt;");
    expect(html).toContain("?q=&lt;public data-secret=&quot;url&quot;&gt;");
    expect(html).not.toContain("<script");
    expect(html).not.toContain("<img src=x");
  });

  it("never serializes the token draft and only reports stored tokens as Keychain state", () => {
    const root = renderDom(
      model({
        wizard: wizard({
          step: 2,
          tokenStored: true,
          tokenDraft: 'never-render-this-token<>&"',
        }),
      }),
    );
    const tokenInput = root.querySelector<HTMLInputElement>('input[name="tunnelToken"]');

    expect(tokenInput?.getAttribute("value")).toBe("");
    expect(root.innerHTML).not.toContain("never-render-this-token");
    expect(root.textContent).toContain("已安全保存在 Keychain");
  });

  it("shows a non-blocking degraded recheck without failed recovery actions", () => {
    const root = renderDom(
      model({
        wizard: wizard({ step: 3 }),
        remoteAccess: {
          ...model().remoteAccess,
          mode: "named",
          named: {
            ...model().remoteAccess.named,
            status: "degraded",
            detail: "public health check is delayed",
          },
        },
      }),
    );
    const actions = Array.from(root.querySelectorAll<HTMLElement>("[data-action]"), (element) =>
      element.dataset.action,
    );

    expect(root.textContent).toContain("立即重新检测");
    expect(actions).toContain("recheck-named-tunnel-health");
    expect(actions).not.toContain("retry-named-tunnel");
    expect(actions).not.toContain("edit-named-tunnel");
    expect(actions).not.toContain("start-temporary-tunnel");
  });

  it("shows exactly four failed actions and keeps the fixed-domain mode selected", () => {
    const root = renderDom(
      model({
        selectedMode: "named",
        wizard: wizard({ step: 3 }),
        remoteAccess: {
          ...model().remoteAccess,
          mode: "named_failed",
          named: {
            ...model().remoteAccess.named,
            status: "failed",
            failureKind: "invalid_configuration",
            detail: "hostname does not match tunnel",
          },
        },
      }),
    );
    const buttonActions = Array.from(
      root.querySelectorAll<HTMLButtonElement>("button[data-action]"),
      (button) => button.dataset.action,
    );
    const namedMode = root.querySelector<HTMLInputElement>('input[name="remoteMode"][value="named"]');
    const quickMode = root.querySelector<HTMLInputElement>('input[name="remoteMode"][value="quick"]');

    expect(buttonActions).toEqual([
      "retry-named-tunnel",
      "edit-named-tunnel",
      "copy-diagnostics",
      "start-temporary-tunnel",
    ]);
    expect(root.textContent).toContain("重试");
    expect(root.textContent).toContain("修改配置");
    expect(root.textContent).toContain("查看诊断");
    expect(root.textContent).toContain("启动临时通道");
    expect(namedMode?.checked).toBe(true);
    expect(quickMode?.checked).toBe(false);
  });

  it("warns when Quick is running and never presents the named tunnel as ready", () => {
    const root = renderDom(
      model({
        selectedMode: "quick",
        remoteAccess: {
          ...model().remoteAccess,
          mode: "quick",
          named: {
            ...model().remoteAccess.named,
            status: "ready",
            publicUrl: "https://fixed.example.com",
          },
          quick: {
            status: "ready",
            publicUrl: "https://temporary.trycloudflare.com",
            localUrl: "http://127.0.0.1:43123",
            detail: null,
          },
        },
      }),
    );

    expect(root.textContent).toContain("当前为临时 URL；锁屏通知已暂停");
    expect(root.textContent).not.toContain("Named Ready");
    expect(root.textContent).not.toContain("https://fixed.example.com");
  });

  it("keeps the temporary-channel warning visible while inspecting fixed settings", () => {
    const root = renderDom(
      model({
        selectedMode: "named",
        remoteAccess: {
          ...model().remoteAccess,
          mode: "quick",
          quick: {
            status: "ready",
            publicUrl: "https://temporary.trycloudflare.com",
            localUrl: "http://127.0.0.1:43123",
            detail: null,
          },
        },
      }),
    );

    expect(root.textContent).toContain("当前为临时 URL；锁屏通知已暂停");
    expect(root.querySelector<HTMLInputElement>('input[value="named"]')?.checked).toBe(true);
  });
});
