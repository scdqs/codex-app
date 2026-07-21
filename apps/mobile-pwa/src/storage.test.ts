import { beforeEach, describe, expect, it } from "vitest";
import {
  loadProjectViewPreferences,
  saveProjectViewPreferences,
} from "./storage";

describe("project view preference storage", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("round trips collapsed projects and locally pinned threads", () => {
    saveProjectViewPreferences({
      collapsedProjectIds: ["/repo/app"],
      pinnedThreadIds: ["thread-1"],
    });

    expect(loadProjectViewPreferences()).toEqual({
      collapsedProjectIds: ["/repo/app"],
      pinnedThreadIds: ["thread-1"],
    });
  });

  it("falls back to empty preferences for malformed or invalid storage", () => {
    localStorage.setItem("codex.mobilePwa.projectView.v1", "not-json");
    expect(loadProjectViewPreferences()).toEqual({
      collapsedProjectIds: [],
      pinnedThreadIds: [],
    });

    localStorage.setItem(
      "codex.mobilePwa.projectView.v1",
      JSON.stringify({ collapsedProjectIds: [1], pinnedThreadIds: "thread-1" }),
    );
    expect(loadProjectViewPreferences()).toEqual({
      collapsedProjectIds: [],
      pinnedThreadIds: [],
    });
  });
});
