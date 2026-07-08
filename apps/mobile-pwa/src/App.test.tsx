import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import App from "./App";

describe("App", () => {
  it("renders the mobile workbench regions and selected session detail", async () => {
    const user = userEvent.setup();
    render(<App />);

    expect(screen.getByLabelText("Connection status")).toHaveTextContent("Connected");
    expect(screen.getByRole("heading", { name: "Pending approvals" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Sessions" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Mobile bridge MVP" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Mobile bridge MVP")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject Run npm install" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve Run npm install" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject Create PWA scaffold" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Approve Create PWA scaffold" })).toBeInTheDocument();

    expect(screen.getByRole("button", { name: /Mobile bridge MVP/ })).toHaveAttribute("aria-current", "true");

    await user.click(screen.getByRole("button", { name: /Bridge sidecar API/ }));

    expect(screen.getByRole("heading", { name: "Bridge sidecar API" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Bridge sidecar API")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Bridge sidecar API/ })).toHaveAttribute("aria-current", "true");
  });
});
