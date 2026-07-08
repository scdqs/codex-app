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

    await user.click(screen.getByRole("button", { name: /Bridge sidecar API/ }));

    expect(screen.getByRole("heading", { name: "Bridge sidecar API" })).toBeInTheDocument();
    expect(screen.getByPlaceholderText("Message Bridge sidecar API")).toBeInTheDocument();
  });
});
