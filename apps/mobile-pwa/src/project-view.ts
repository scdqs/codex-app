import type { SessionSnapshot } from "@codex/bridge-protocol";

const OTHER_PROJECT_ID = "__other_sessions__";

export interface SessionProjectGroup {
  id: string;
  label: string;
  cwd?: string;
  sessions: SessionSnapshot[];
  latestUpdatedAt: number;
}

export function groupSessionsByProject(
  sessions: SessionSnapshot[],
  pinnedThreadIds: ReadonlySet<string>,
): SessionProjectGroup[] {
  const groups = new Map<string, SessionProjectGroup>();

  for (const session of sessions) {
    const cwd = normalizeProjectCwd(session.cwd);
    const id = cwd ?? OTHER_PROJECT_ID;
    const current = groups.get(id) ?? {
      id,
      label: cwd ? projectLabel(cwd) : "Other sessions",
      cwd,
      sessions: [],
      latestUpdatedAt: 0,
    };
    current.sessions.push(session);
    current.latestUpdatedAt = Math.max(current.latestUpdatedAt, session.updatedAt);
    groups.set(id, current);
  }

  return [...groups.values()]
    .map((group) => ({
      ...group,
      sessions: [...group.sessions].sort((left, right) => {
        const pinDifference = Number(pinnedThreadIds.has(right.threadId)) - Number(pinnedThreadIds.has(left.threadId));
        return pinDifference || right.updatedAt - left.updatedAt || left.threadId.localeCompare(right.threadId);
      }),
    }))
    .sort((left, right) => {
      if (left.id === OTHER_PROJECT_ID) {
        return 1;
      }
      if (right.id === OTHER_PROJECT_ID) {
        return -1;
      }
      return right.latestUpdatedAt - left.latestUpdatedAt || left.label.localeCompare(right.label);
    });
}

export function normalizeProjectCwd(cwd?: string): string | undefined {
  const trimmed = cwd?.trim();
  if (!trimmed) {
    return undefined;
  }
  if (trimmed === "/" || /^[A-Za-z]:[\\/]?$/.test(trimmed)) {
    return trimmed.replace("\\", "/");
  }
  return trimmed.replace(/[\\/]+$/, "").replaceAll("\\", "/");
}

function projectLabel(cwd: string): string {
  if (cwd === "/") {
    return "/";
  }
  const segments = cwd.split("/").filter(Boolean);
  return segments.at(-1) ?? cwd;
}
