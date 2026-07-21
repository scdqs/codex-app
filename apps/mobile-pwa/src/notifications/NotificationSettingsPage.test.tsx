import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { NotificationSettingsPage } from "./NotificationSettingsPage";

describe("NotificationSettingsPage", () => {
  it("renders master, four kinds, sound, vibration and test controls", () => {
    render(<NotificationSettingsPage {...props()} />);

    for (const name of [
      "Task alerts",
      "Completed alerts",
      "Approval required alerts",
      "Input required alerts",
      "Error alerts",
      "Sound",
      "Vibration",
    ]) {
      expect(screen.getByRole("switch", { name })).toBeInTheDocument();
    }
    expect(screen.getByRole("button", { name: "Send test alert" })).toBeInTheDocument();
    expect(screen.getByText("Unavailable")).toBeInTheDocument();
  });

  it("preview calls the selected tone even when sound is off", async () => {
    const onPreview = vi.fn();
    render(<NotificationSettingsPage {...props()} onPreview={onPreview} settings={{ ...props().settings, soundEnabled: false }} />);

    await userEvent.click(screen.getByRole("button", { name: "Preview error sound" }));

    expect(onPreview).toHaveBeenCalledWith("error");
  });

  it("offers_enable_repair_and_disable_for_the_matching_system_state", () => {
    const { rerender } = render(
      <NotificationSettingsPage {...props()} systemNotificationState="not_enabled" />,
    );
    expect(screen.getByRole("button", { name: "Enable system notifications" })).toBeInTheDocument();

    rerender(<NotificationSettingsPage {...props()} systemNotificationState="needs_repair" />);
    expect(screen.getByRole("button", { name: "Repair notifications" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Disable alerts" })).toBeInTheDocument();

    rerender(<NotificationSettingsPage {...props()} systemNotificationState="active" />);
    expect(screen.getByText("Active")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Disable alerts" })).toBeInTheDocument();
  });

  it("explains_ios_install_and_system_control_without_enabling_vibration_toggle", () => {
    render(
      <NotificationSettingsPage
        {...props()}
        browserCapabilities={{
          ...props().browserCapabilities,
          fixedHttps: true,
          isIos: true,
          standalone: false,
        }}
        capabilities={{
          ...props().capabilities,
          fixedHttps: true,
          vibrationControlledBySystem: true,
        }}
      />,
    );

    expect(screen.getByText(/add this app to the home screen/i)).toBeInTheDocument();
    expect(screen.getByText(/controlled by the iphone system/i)).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Vibration" })).toBeDisabled();
  });
});

function props() {
  return {
    busy: false,
    capabilities: {
      deliveryMode: "foreground_only" as const,
      fixedHttps: false,
      systemNotifications: false,
      foregroundSound: true,
      foregroundVibration: false,
      vibrationControlledBySystem: false,
    },
    browserCapabilities: {
      fixedHttps: false,
      secureContext: true,
      serviceWorker: true,
      pushManager: true,
      notificationApi: true,
      isIos: false,
      standalone: true,
    },
    error: "",
    onBack: vi.fn(),
    onChange: vi.fn(),
    onDisableAlerts: vi.fn(),
    onEnableSystemNotifications: vi.fn(),
    onPreview: vi.fn(),
    onRepairSystemNotifications: vi.fn(),
    onSendTest: vi.fn(),
    systemNotificationState: "unavailable" as const,
    settings: {
      enabled: false,
      alertKinds: {
        completed: true,
        approvalRequired: true,
        inputRequired: true,
        error: true,
      },
      soundEnabled: true,
      vibrationEnabled: true,
    },
  };
}
