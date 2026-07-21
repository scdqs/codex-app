import { describe, expect, it } from "vitest";
import type { SessionSnapshot } from "@codex/bridge-protocol";
import { groupSessionsByProject, normalizeProjectCwd } from "./project-view";

describe("project view", () => {
  it("groups normalized cwd values and keeps uncategorized sessions separate", () => {
    const groups = groupSessionsByProject(
      [
        session({ threadId: "thread-app-old", cwd: "/repo/app/", updatedAt: 100 }),
        session({ threadId: "thread-other", cwd: "/repo/other", updatedAt: 80 }),
        session({ threadId: "thread-app-new", cwd: "/repo/app", updatedAt: 200 }),
        session({ threadId: "thread-loose", cwd: undefined, updatedAt: 50 }),
      ],
      new Set(),
    );

    expect(groups.map((group) => [group.id, group.label, group.sessions.length])).toEqual([
      ["/repo/app", "app", 2],
      ["/repo/other", "other", 1],
      ["__other_sessions__", "Other sessions", 1],
    ]);
    expect(groups[0].sessions.map((item) => item.threadId)).toEqual([
      "thread-app-new",
      "thread-app-old",
    ]);
  });

  it("sorts locally pinned sessions before newer unpinned sessions", () => {
    const groups = groupSessionsByProject(
      [
        session({ threadId: "thread-new", cwd: "/repo/app", updatedAt: 200 }),
        session({ threadId: "thread-pinned", cwd: "/repo/app", updatedAt: 100 }),
      ],
      new Set(["thread-pinned"]),
    );

    expect(groups[0].sessions.map((item) => item.threadId)).toEqual([
      "thread-pinned",
      "thread-new",
    ]);
  });

  it("normalizes trailing separators without changing the filesystem root", () => {
    expect(normalizeProjectCwd("/repo/app///")).toBe("/repo/app");
    expect(normalizeProjectCwd("/")).toBe("/");
    expect(normalizeProjectCwd("  ")).toBeUndefined();
  });
});

function session(overrides: Partial<SessionSnapshot>): SessionSnapshot {
  return {
    threadId: "thread-1",
    title: "Session",
    preview: "Preview",
    updatedAt: 1,
    status: "idle",
    pendingApprovalIds: [],
    ...overrides,
  };
}
