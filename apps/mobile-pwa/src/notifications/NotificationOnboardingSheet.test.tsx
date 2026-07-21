import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { NotificationOnboardingSheet } from "./NotificationOnboardingSheet";
import {
  dismissNotificationOnboarding,
  hasDismissedNotificationOnboarding,
} from "./onboarding-storage";

describe("NotificationOnboardingSheet", () => {
  beforeEach(() => localStorage.clear());

  it("supports dismissing onboarding for the current device", async () => {
    const onNotNow = vi.fn(() => dismissNotificationOnboarding("phone-1"));
    render(
      <NotificationOnboardingSheet
        busy={false}
        error=""
        fixedHttps={false}
        isIos={false}
        onEnable={vi.fn()}
        onNotNow={onNotNow}
        standalone={true}
      />,
    );

    await userEvent.click(screen.getByRole("button", { name: "Not now" }));

    expect(onNotNow).toHaveBeenCalled();
    expect(hasDismissedNotificationOnboarding("phone-1")).toBe(true);
    expect(screen.getByText(/foreground only/i)).toBeInTheDocument();
  });

  it("shows_home_screen_instructions_for_iphone_without_hiding_the_choice", () => {
    render(
      <NotificationOnboardingSheet
        busy={false}
        error="Add this app to the Home Screen before enabling system notifications"
        fixedHttps
        isIos
        onEnable={vi.fn()}
        onNotNow={vi.fn()}
        standalone={false}
      />,
    );

    expect(screen.getByText(/share.*add to home screen.*reopen/i)).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent(/home screen/i);
    expect(screen.getByRole("button", { name: "Enable alerts" })).toBeEnabled();
  });
});
