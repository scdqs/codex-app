# Mobile Session Drawer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the PWA Sessions list into a mobile-only left drawer while preserving the desktop two-column workbench.

**Architecture:** Keep `SessionList` reusable and pure. `App` owns drawer open state, `ConnectionBar` exposes the mobile menu trigger, and a new `SessionDrawer` wrapper reuses `SessionList` with close/backdrop/Escape behavior.

**Tech Stack:** React, TypeScript, Vitest, Testing Library, CSS media queries, lucide-react icons.

---

## File Structure

- Modify `apps/mobile-pwa/src/App.tsx`: add drawer state, extend `ConnectionBar`, add `SessionDrawer`, reuse `SessionList`, close drawer on session selection and Escape.
- Modify `apps/mobile-pwa/src/styles.css`: add desktop-hidden mobile drawer classes, convert mobile header to three columns, remove mobile Sessions row from `.session-grid`.
- Modify `apps/mobile-pwa/src/App.test.tsx`: update the existing render assertion for duplicate Sessions headings, add drawer behavior tests, add CSS assertions for mobile layout.

## Task 1: Drawer Interaction Tests

**Files:**
- Modify: `apps/mobile-pwa/src/App.test.tsx`

- [ ] **Step 1: Update the baseline render test so it still verifies desktop Sessions**

Replace the first Sessions heading assertion in `renders the mobile workbench regions and selected session detail` with:

```tsx
expect(screen.getByRole("button", { name: "Open sessions" })).toBeInTheDocument();
expect(screen.getByRole("heading", { name: "Sessions" })).toBeInTheDocument();
expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
```

- [ ] **Step 2: Add the drawer open/close behavior test**

Insert this test after `renders the mobile workbench regions and selected session detail`:

```tsx
it("opens_and_closes_the_mobile_session_drawer", async () => {
  const user = userEvent.setup();

  render(<App />);

  await user.click(screen.getByRole("button", { name: "Open sessions" }));

  expect(screen.getByRole("dialog", { name: "Sessions" })).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Close sessions" })).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "Close sessions" }));

  expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
});
```

- [ ] **Step 3: Add the backdrop close test**

Insert this test after the open/close test:

```tsx
it("closes_the_mobile_session_drawer_from_the_backdrop", async () => {
  const user = userEvent.setup();

  render(<App />);

  await user.click(screen.getByRole("button", { name: "Open sessions" }));
  await user.click(screen.getByLabelText("Close sessions drawer"));

  expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
});
```

- [ ] **Step 4: Add the select-and-close test**

Insert this test after the backdrop test:

```tsx
it("selects_a_session_from_the_mobile_drawer_and_closes_it", async () => {
  const user = userEvent.setup();

  render(<App />);

  await user.click(screen.getByRole("button", { name: "Open sessions" }));
  const drawer = screen.getByRole("dialog", { name: "Sessions" });
  await user.click(within(drawer).getByRole("button", { name: /Bridge sidecar API/ }));

  expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
  expect(screen.getByRole("heading", { name: "Bridge sidecar API" })).toBeInTheDocument();
  expect(screen.getByPlaceholderText("Message Bridge sidecar API")).toBeInTheDocument();
});
```

Also update the import:

```tsx
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
```

- [ ] **Step 5: Add the Escape close test**

Insert this test after the select-and-close test:

```tsx
it("closes_the_mobile_session_drawer_with_escape", async () => {
  const user = userEvent.setup();

  render(<App />);

  await user.click(screen.getByRole("button", { name: "Open sessions" }));
  await user.keyboard("{Escape}");

  expect(screen.queryByRole("dialog", { name: "Sessions" })).not.toBeInTheDocument();
});
```

- [ ] **Step 6: Run tests to verify failure before implementation**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx
```

Expected: FAIL because `Open sessions`, drawer dialog, and related classes are not implemented yet.

## Task 2: React Drawer Components

**Files:**
- Modify: `apps/mobile-pwa/src/App.tsx`

- [ ] **Step 1: Add the missing icon import**

Change the lucide import block so it includes `Menu`:

```tsx
  FilePenLine,
  Menu,
  Send,
```

- [ ] **Step 2: Add drawer state and handlers in `App`**

Add state after `draft`:

```tsx
const [isSessionDrawerOpen, setIsSessionDrawerOpen] = useState(false);
```

Add this handler before the `return`:

```tsx
function handleSelectSession(threadId: string) {
  setSelectedThreadId(threadId);
  setIsSessionDrawerOpen(false);
}
```

- [ ] **Step 3: Wire `ConnectionBar`, desktop `SessionList`, and `SessionDrawer`**

Replace the current `ConnectionBar` call with:

```tsx
<ConnectionBar
  connection={connection}
  statusText={statusText}
  showSessionMenuButton
  onOpenSessions={() => setIsSessionDrawerOpen(true)}
/>
```

Replace the `SessionList` inside `.session-grid` with:

```tsx
<div className="desktop-session-panel">
  <SessionList
    sessions={sessions}
    selectedThreadId={selectedSession?.threadId ?? ""}
    onSelect={setSelectedThreadId}
  />
</div>
```

Add this after `</section>` for `.workbench` and before `<Composer ...>`:

```tsx
<SessionDrawer
  open={isSessionDrawerOpen}
  sessions={sessions}
  selectedThreadId={selectedSession?.threadId ?? ""}
  onClose={() => setIsSessionDrawerOpen(false)}
  onSelect={handleSelectSession}
/>
```

- [ ] **Step 4: Extend `ConnectionBar` props and render the menu trigger**

Replace the `ConnectionBar` signature with:

```tsx
function ConnectionBar({
  connection,
  onOpenSessions,
  showSessionMenuButton = false,
  statusText,
}: {
  connection: ConnectionViewState;
  onOpenSessions?: () => void;
  showSessionMenuButton?: boolean;
  statusText: string;
}) {
```

Then render the menu button as the first child of `<header>`:

```tsx
{showSessionMenuButton ? (
  <button
    className="session-menu-button"
    onClick={onOpenSessions}
    type="button"
    aria-label="Open sessions"
  >
    <Menu size={18} aria-hidden="true" />
  </button>
) : null}
```

- [ ] **Step 5: Add `SessionDrawer` below `ApprovalQueue` and above `SessionList`**

Add:

```tsx
function SessionDrawer({
  onClose,
  onSelect,
  open,
  selectedThreadId,
  sessions,
}: {
  onClose: () => void;
  onSelect: (threadId: string) => void;
  open: boolean;
  selectedThreadId: string;
  sessions: SessionSnapshot[];
}) {
  const closeButtonRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!open) {
      return;
    }

    closeButtonRef.current?.focus();

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        onClose();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [onClose, open]);

  if (!open) {
    return null;
  }

  return (
    <div className="session-drawer-layer">
      <button
        className="session-drawer-backdrop"
        onClick={onClose}
        type="button"
        aria-label="Close sessions drawer"
      />
      <aside className="session-drawer" role="dialog" aria-modal="true" aria-label="Sessions">
        <div className="drawer-heading">
          <div>
            <p className="eyebrow">Switch thread</p>
            <h2>Sessions</h2>
          </div>
          <div className="drawer-heading-actions">
            <span>{sessions.length}</span>
            <button
              className="icon-button"
              onClick={onClose}
              ref={closeButtonRef}
              type="button"
              aria-label="Close sessions"
            >
              <X size={16} aria-hidden="true" />
            </button>
          </div>
        </div>
        <SessionList sessions={sessions} selectedThreadId={selectedThreadId} onSelect={onSelect} />
      </aside>
    </div>
  );
}
```

- [ ] **Step 6: Run drawer tests**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx
```

Expected: drawer behavior tests PASS or fail only on CSS assertions added in Task 3.

## Task 3: Mobile CSS

**Files:**
- Modify: `apps/mobile-pwa/src/styles.css`

- [ ] **Step 1: Add base classes for the menu button and drawer**

Add after `.connection-bar`:

```css
.session-menu-button {
  display: none;
}
```

Add after `.session-list-panel, .session-detail` rules:

```css
.desktop-session-panel {
  display: flex;
  min-height: 0;
}

.desktop-session-panel > .session-list-panel {
  flex: 1;
}

.session-drawer-layer {
  display: none;
}
```

- [ ] **Step 2: Replace the mobile header and grid rules**

In `@media (max-width: 720px)`, replace the current `.connection-bar`, `.connection-meta`, and `.session-grid` rules with:

```css
.connection-bar {
  display: grid;
  grid-template-columns: 38px minmax(0, 1fr) minmax(0, auto);
  align-items: center;
  gap: 10px;
  min-height: calc(58px + var(--safe-top));
}

.session-menu-button {
  display: grid;
  width: 38px;
  height: 38px;
  place-items: center;
  border: 1px solid var(--border-strong);
  border-radius: 8px;
  background: var(--surface-strong);
  color: var(--text-soft);
}

.connection-primary {
  min-width: 0;
}

.connection-primary > div {
  min-width: 0;
}

.connection-primary h1,
.connection-detail {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.connection-meta {
  max-width: 46vw;
  justify-content: flex-end;
}

.desktop-session-panel {
  display: none;
}

.session-grid {
  grid-template-columns: 1fr;
  grid-template-rows: minmax(0, 1fr);
}
```

- [ ] **Step 3: Add mobile drawer CSS inside the same media query**

Add below the `.session-grid` mobile rule:

```css
.session-drawer-layer {
  position: fixed;
  inset: 0;
  z-index: 30;
  display: block;
}

.session-drawer-backdrop {
  position: absolute;
  inset: 0;
  width: 100%;
  border: 0;
  background: rgb(0 0 0 / 48%);
}

.session-drawer {
  position: absolute;
  top: 0;
  bottom: 0;
  left: 0;
  display: flex;
  width: min(84vw, 340px);
  min-width: 280px;
  flex-direction: column;
  border-right: 1px solid var(--border);
  background: var(--surface);
  box-shadow: 22px 0 54px rgb(0 0 0 / 36%);
}

.drawer-heading {
  display: flex;
  min-height: calc(58px + var(--safe-top));
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: calc(10px + var(--safe-top)) 10px 10px 12px;
  border-bottom: 1px solid var(--border);
}

.drawer-heading-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.drawer-heading-actions > span {
  color: var(--text-muted);
  font-size: 0.78rem;
  font-weight: 750;
}

.session-drawer .session-list-panel {
  min-height: 0;
  flex: 1;
  border: 0;
  border-radius: 0;
  box-shadow: none;
}

.session-drawer .session-list-panel > .section-heading {
  display: none;
}
```

- [ ] **Step 4: Run CSS-focused test**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx -t independent_scroll
```

Expected: PASS after updating the CSS expectations in Task 4.

## Task 4: CSS Regression Assertions

**Files:**
- Modify: `apps/mobile-pwa/src/App.test.tsx`

- [ ] **Step 1: Extend `uses_independent_scroll_containers_for_sessions_and_events`**

Add these assertions at the end of the test:

```tsx
expect(css).toContain(".desktop-session-panel");
expect(css).toContain(".session-drawer-layer");
expect(css).toContain("grid-template-columns: minmax(0, 0.95fr) minmax(0, 1.45fr)");
expect(css).toContain("grid-template-rows: minmax(0, 1fr)");
expect(css).not.toContain("grid-template-rows: minmax(150px, 0.42fr) minmax(0, 1fr)");
```

- [ ] **Step 2: Add a targeted mobile layout CSS test**

Insert this test after `uses_independent_scroll_containers_for_sessions_and_events`:

```tsx
it("defines_mobile_session_drawer_layout_without_a_stacked_session_panel", () => {
  const stylesUrl = new URL("./styles.css", import.meta.url);
  const stylesPath =
    stylesUrl.protocol === "file:"
      ? stylesUrl
      : stylesUrl.pathname.startsWith("/@fs/")
        ? stylesUrl.pathname.slice("/@fs".length)
        : `.${stylesUrl.pathname}`;
  const css = readFileSync(stylesPath, "utf8");

  expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.connection-bar\s*\{[\s\S]*grid-template-columns:\s*38px minmax\(0, 1fr\) minmax\(0, auto\);/);
  expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.session-menu-button\s*\{[\s\S]*display:\s*grid;/);
  expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.desktop-session-panel\s*\{[\s\S]*display:\s*none;/);
  expect(css).toMatch(/@media \(max-width: 720px\)[\s\S]*\.session-drawer\s*\{[\s\S]*width:\s*min\(84vw, 340px\);/);
  expect(css).not.toContain("grid-template-rows: minmax(150px, 0.42fr) minmax(0, 1fr)");
});
```

- [ ] **Step 3: Run the full PWA test suite**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run
```

Expected: all tests PASS.

## Task 5: Build And Browser Verification

**Files:**
- No code edits unless verification finds a defect.

- [ ] **Step 1: Build the PWA**

Run:

```bash
cd apps/mobile-pwa && npm run build
```

Expected: build completes without TypeScript or Vite errors.

- [ ] **Step 2: Verify desktop layout in browser**

Open the running bridge URL at a desktop viewport and confirm:

- The persistent Sessions panel is visible on the left.
- Session detail remains visible on the right.
- Header chips stay aligned to the right.
- Console has no new React or CSS errors.

- [ ] **Step 3: Verify mobile layout in browser**

Open the same URL at a phone-width viewport and confirm:

- The persistent Sessions panel is not visible by default.
- The top-left `Open sessions` icon opens the drawer.
- The drawer width is `min(84vw, 340px)` visually and covers the main content with a backdrop.
- Selecting a session closes the drawer and updates the detail title.
- The composer stays anchored at the bottom and is only covered by the drawer/backdrop while open.

- [ ] **Step 4: Commit implementation**

Run:

```bash
git add apps/mobile-pwa/src/App.tsx apps/mobile-pwa/src/styles.css apps/mobile-pwa/src/App.test.tsx docs/superpowers/plans/2026-07-09-mobile-session-drawer.md
git commit -m "feat: add mobile session drawer"
```

Expected: commit succeeds on `feature/codex-mobile-bridge-mvp`.

## Self-Review

- Spec coverage: covered mobile-only drawer, header button/status alignment, desktop preservation, close/backdrop/Escape/select interactions, tests, build, and browser checks.
- Placeholder scan: no placeholder markers or vague test instructions remain.
- Type consistency: `SessionDrawer` props match `SessionSnapshot`, `selectedThreadId`, `onSelect`, and `onClose` usage in `App`; `ConnectionBar` optional props match the call site.
