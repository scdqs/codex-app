# Mobile PWA Thread UI Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 Codex Mobile PWA 的会话列表滚动、消息展示顺序、底部跟随滚动和本机图片附件显示。

**Architecture:** 前端继续以 React PWA 作为唯一手机 UI，统一在事件合并出口按 Codex Desktop 风格做旧到新排序，并让 Sessions panel 与 event stream 各自滚动。后端 normalizer 提取 `localImage` 为内部附件，sidecar 注册本机图片路径并只向 PWA 返回受 token 映射保护的 asset URL；前端用已有 session token fetch 图片 blob 后渲染缩略图。

**Tech Stack:** Rust workspace、Axum、Tokio、serde_json、React 19、TypeScript、Vite、Vitest、Testing Library。

---

## Scope Check

本计划只覆盖 `docs/superpowers/specs/2026-07-09-mobile-pwa-thread-ui-design.md` 中的 thread UI 和本机图片代理。Codex Desktop CDP 注入、配对 URL、短轮询机制、公网 tunnel、复杂授权策略不在本计划内。

## File Structure

- Modify: `apps/mobile-pwa/src/App.tsx`
  - 负责事件排序、pending echo 合并、SessionDetail 滚动行为、event row 附件渲染。
- Modify: `apps/mobile-pwa/src/App.test.tsx`
  - 覆盖旧到新排序、独立滚动 CSS、底部跟随、图片附件渲染和授权 fetch。
- Modify: `apps/mobile-pwa/src/api.ts`
  - 新增 `fetchAssetBlob`，统一为图片代理请求附带 bearer token。
- Modify: `apps/mobile-pwa/src/protocol.ts`
  - 新增图片附件 payload 类型，保持 `SessionEvent.payload` 兼容 `JsonValue`。
- Modify: `apps/mobile-pwa/src/styles.css`
  - 固定 workbench 高度分配，让 left sessions 与 right event stream 独立滚动；增加附件缩略图样式。
- Modify: `crates/bridge-core/src/lib.rs`
  - 暴露新的 local asset registry 模块。
- Create: `crates/bridge-core/src/local_assets.rs`
  - 管理本机图片路径到随机 asset token 的映射，避免任意路径读取。
- Modify: `crates/bridge-core/src/normalizer.rs`
  - 从 Codex raw `localImage` 中提取内部图片附件。
- Modify: `crates/bridge-core/src/http_api.rs`
  - 注册附件、擦除本机路径、返回 asset URL，并提供授权图片代理接口。

---

### Task 1: Codex-Style Message Order And Independent Scroll Containers

**Files:**
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Modify: `apps/mobile-pwa/src/styles.css`

- [ ] **Step 1: Write failing ordering and CSS tests**

In `apps/mobile-pwa/src/App.test.tsx`, change the React Testing Library import to include `fireEvent`, and add `readFileSync`:

```ts
import { readFileSync } from "node:fs";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
```

Replace the existing `keeps_polled_events_newest_first_and_reconciles_pending_echo_with_newline` test with:

```ts
it("keeps_polled_events_oldest_first_and_reconciles_pending_echo_with_newline", () => {
  const current = [
    sessionEvent({
      id: "old-assistant",
      threadId: "thread-a",
      payload: { role: "assistant", text: "Old answer" },
      createdAt: 1_783_515_380_000,
    }),
    sessionEvent({
      id: "local-pending",
      threadId: "thread-a",
      payload: { role: "user", text: "continue", pending: true },
      createdAt: 1_783_515_390_000,
    }),
  ];
  const polled = [
    sessionEvent({
      id: "turn-new:item-1",
      threadId: "thread-a",
      payload: { role: "user", text: "continue\n" },
      createdAt: 1_783_515_391_000,
    }),
    sessionEvent({
      id: "turn-new:item-2",
      threadId: "thread-a",
      payload: { role: "assistant", text: "New answer" },
      createdAt: 1_783_515_391_000,
    }),
    current[0],
  ];

  const merged = mergePolledSessionEvents(current, polled);

  expect(merged.map((event) => event.id)).toEqual([
    "old-assistant",
    "turn-new:item-1",
    "turn-new:item-2",
  ]);
  expect(
    merged
      .map((event) => event.payload)
      .filter(
        (payload) =>
          payload &&
          typeof payload === "object" &&
          "text" in payload &&
          payload.text === "continue",
      ),
  ).toHaveLength(0);
});
```

Add this CSS regression test near the other rendering tests:

```ts
it("uses_independent_scroll_containers_for_sessions_and_events", () => {
  const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

  expect(css).toContain(".workbench");
  expect(css).toContain("overflow: hidden");
  expect(css).toContain(".session-list,");
  expect(css).toContain(".event-stream");
  expect(css).toContain("overflow-y: auto");
  expect(css).toContain(".session-list-panel,");
  expect(css).toContain("flex-direction: column");
});
```

- [ ] **Step 2: Run the targeted frontend tests and confirm failure**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx -t "oldest_first|independent_scroll"
```

Expected: the ordering test fails because `sortSessionEvents` currently sorts newest first. The CSS test fails because `.workbench` still uses `overflow-y: auto` and the panels are not flex columns.

- [ ] **Step 3: Implement old-to-new event sorting**

In `apps/mobile-pwa/src/App.tsx`, replace `sortSessionEvents` with:

```ts
function sortSessionEvents(events: SessionEvent[]): SessionEvent[] {
  return [...events].sort((left, right) => left.createdAt - right.createdAt);
}
```

This keeps `appendOrMergeSessionEvent` compatible with assistant deltas because the newest event remains the last item after sorting.

- [ ] **Step 4: Implement independent scroll CSS**

In `apps/mobile-pwa/src/styles.css`, replace the `.workbench` block with:

```css
.workbench {
  display: grid;
  grid-template-rows: auto minmax(0, 1fr);
  min-height: 0;
  overflow: hidden;
  padding: 12px 12px calc(var(--composer-height) + var(--safe-bottom) + 14px);
}
```

Add this block after the shared card styles:

```css
.session-list-panel,
.session-detail {
  display: flex;
  min-height: 0;
  flex-direction: column;
}
```

Replace the `.session-grid` block with:

```css
.session-grid {
  display: grid;
  grid-template-columns: minmax(0, 0.95fr) minmax(0, 1.45fr);
  gap: 12px;
  min-height: 0;
  overflow: hidden;
}
```

Replace the `.session-list, .event-stream` block with:

```css
.session-list,
.event-stream {
  display: grid;
  min-height: 0;
  gap: 1px;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 8px;
}
```

Add:

```css
.session-list {
  flex: 1;
}

.event-stream {
  flex: 1;
  align-content: start;
}
```

Inside `@media (max-width: 720px)`, replace the `.session-grid` block with:

```css
.session-grid {
  grid-template-columns: 1fr;
  grid-template-rows: minmax(150px, 0.42fr) minmax(0, 1fr);
}
```

- [ ] **Step 5: Run the targeted frontend tests and confirm pass**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx -t "oldest_first|independent_scroll"
```

Expected: both targeted tests pass.

- [ ] **Step 6: Commit Task 1**

```bash
git add apps/mobile-pwa/src/App.tsx apps/mobile-pwa/src/App.test.tsx apps/mobile-pwa/src/styles.css
git commit -m "fix: show pwa events oldest first"
```

---

### Task 2: Bottom-Stick Scroll Behavior

**Files:**
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`

- [ ] **Step 1: Write failing scroll behavior tests**

In `apps/mobile-pwa/src/App.test.tsx`, add this helper near `sessionEvent`:

```ts
function setScrollMetrics(
  element: Element,
  metrics: { scrollTop: number; clientHeight: number; scrollHeight: number },
) {
  Object.defineProperty(element, "scrollTop", {
    configurable: true,
    writable: true,
    value: metrics.scrollTop,
  });
  Object.defineProperty(element, "clientHeight", {
    configurable: true,
    value: metrics.clientHeight,
  });
  Object.defineProperty(element, "scrollHeight", {
    configurable: true,
    value: metrics.scrollHeight,
  });
}
```

Add this test:

```ts
it("keeps_event_stream_at_bottom_when_new_events_arrive_near_bottom", async () => {
  saveActiveSession();
  const scrollTo = vi.fn();
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: scrollTo,
  });
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = String(input);
    if (url === "http://bridge.local/api/health") {
      return jsonResponse({ status: "ok", connectionState: "writable" });
    }
    if (url === "http://bridge.local/api/sessions") {
      return jsonResponse([
        sessionSnapshot({ threadId: "thread-live", title: "Live thread", preview: "Real session" }),
      ]);
    }
    if (url === "http://bridge.local/api/sessions/thread-live/events") {
      return jsonResponse([
        sessionEvent({
          id: "event-first",
          threadId: "thread-live",
          payload: { role: "assistant", text: "First" },
          createdAt: 1_783_515_380_000,
        }),
      ]);
    }
    return jsonResponse({});
  });

  render(<App />);

  const stream = await screen.findByLabelText("Session event stream");
  setScrollMetrics(stream, { scrollTop: 720, clientHeight: 200, scrollHeight: 900 });
  fireEvent.scroll(stream);

  act(() => {
    MockWebSocket.instances[0].emit({
      type: "session_event",
      payload: sessionEvent({
        id: "event-second",
        threadId: "thread-live",
        payload: { role: "assistant", text: "Second" },
        createdAt: 1_783_515_390_000,
      }),
    });
  });

  await screen.findByText("Second");
  expect(scrollTo).toHaveBeenLastCalledWith({ top: 900, behavior: "auto" });
});
```

Add this test:

```ts
it("does_not_steal_scroll_when_user_is_reading_history", async () => {
  saveActiveSession();
  const scrollTo = vi.fn();
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value: scrollTo,
  });
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = String(input);
    if (url === "http://bridge.local/api/health") {
      return jsonResponse({ status: "ok", connectionState: "writable" });
    }
    if (url === "http://bridge.local/api/sessions") {
      return jsonResponse([
        sessionSnapshot({ threadId: "thread-live", title: "Live thread", preview: "Real session" }),
      ]);
    }
    if (url === "http://bridge.local/api/sessions/thread-live/events") {
      return jsonResponse([
        sessionEvent({
          id: "event-first",
          threadId: "thread-live",
          payload: { role: "assistant", text: "First" },
          createdAt: 1_783_515_380_000,
        }),
      ]);
    }
    return jsonResponse({});
  });

  render(<App />);

  const stream = await screen.findByLabelText("Session event stream");
  scrollTo.mockClear();
  setScrollMetrics(stream, { scrollTop: 100, clientHeight: 200, scrollHeight: 900 });
  fireEvent.scroll(stream);

  act(() => {
    MockWebSocket.instances[0].emit({
      type: "session_event",
      payload: sessionEvent({
        id: "event-second",
        threadId: "thread-live",
        payload: { role: "assistant", text: "Second" },
        createdAt: 1_783_515_390_000,
      }),
    });
  });

  await screen.findByText("Second");
  expect(scrollTo).not.toHaveBeenCalled();
});
```

- [ ] **Step 2: Run the targeted scroll tests and confirm failure**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx -t "bottom|reading_history"
```

Expected: both tests fail because `SessionDetail` does not own a scroll ref or bottom-stick state.

- [ ] **Step 3: Add scroll refs and bottom-stick logic**

In `apps/mobile-pwa/src/App.tsx`, change the React import to:

```ts
import {
  FormEvent,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
```

At the top of `SessionDetail`, before the early `if (!session)` return, add:

```ts
const eventStreamRef = useRef<HTMLDivElement | null>(null);
const shouldStickToBottomRef = useRef(true);
const previousThreadIdRef = useRef<string | null>(null);
const threadId = session ? session.threadId : "";
const eventTail = sessionEvents.at(-1);
const eventTailKey = eventTail
  ? `${eventTail.id}:${eventTail.createdAt}:${payloadText(eventTail.payload).length}`
  : "";

useLayoutEffect(() => {
  if (!threadId) {
    return;
  }
  const stream = eventStreamRef.current;
  if (!stream) {
    return;
  }

  const threadChanged = previousThreadIdRef.current !== threadId;
  if (threadChanged || shouldStickToBottomRef.current) {
    stream.scrollTo({ top: stream.scrollHeight, behavior: "auto" });
    shouldStickToBottomRef.current = true;
  }
  previousThreadIdRef.current = threadId;
}, [eventTailKey, threadId]);

function handleEventStreamScroll() {
  const stream = eventStreamRef.current;
  if (!stream) {
    return;
  }
  shouldStickToBottomRef.current =
    stream.scrollHeight - stream.scrollTop - stream.clientHeight < 80;
}
```

Update the event stream element to:

```tsx
<div
  className="event-stream"
  aria-label="Session event stream"
  ref={eventStreamRef}
  onScroll={handleEventStreamScroll}
>
```

- [ ] **Step 4: Run the targeted scroll tests and confirm pass**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx -t "bottom|reading_history"
```

Expected: both targeted tests pass.

- [ ] **Step 5: Commit Task 2**

```bash
git add apps/mobile-pwa/src/App.tsx apps/mobile-pwa/src/App.test.tsx
git commit -m "fix: preserve pwa thread scroll position"
```

---

### Task 3: Frontend Image Attachment Rendering

**Files:**
- Modify: `apps/mobile-pwa/src/api.ts`
- Modify: `apps/mobile-pwa/src/protocol.ts`
- Modify: `apps/mobile-pwa/src/App.tsx`
- Modify: `apps/mobile-pwa/src/App.test.tsx`
- Modify: `apps/mobile-pwa/src/styles.css`

- [ ] **Step 1: Write failing attachment render test**

In `apps/mobile-pwa/src/App.test.tsx`, add this URL blob helper near `saveActiveSession`:

```ts
function stubObjectUrls() {
  Object.defineProperty(URL, "createObjectURL", {
    configurable: true,
    value: vi.fn(() => "blob:codex-image"),
  });
  Object.defineProperty(URL, "revokeObjectURL", {
    configurable: true,
    value: vi.fn(),
  });
}
```

Add this test:

```ts
it("renders_image_attachments_with_authenticated_asset_fetch", async () => {
  stubObjectUrls();
  saveActiveSession();
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = String(input);
    if (url === "http://bridge.local/api/health") {
      return jsonResponse({ status: "ok", connectionState: "writable" });
    }
    if (url === "http://bridge.local/api/sessions") {
      return jsonResponse([
        sessionSnapshot({ threadId: "thread-image", title: "Image thread", preview: "Has image" }),
      ]);
    }
    if (url === "http://bridge.local/api/sessions/thread-image/events") {
      return jsonResponse([
        sessionEvent({
          id: "event-image",
          threadId: "thread-image",
          payload: {
            role: "user",
            text: "see attached",
            attachments: [
              {
                type: "image",
                src: "/api/assets/local-image/asset-1",
                name: "codex-clipboard.png",
              },
            ],
          },
        }),
      ]);
    }
    if (url === "http://bridge.local/api/assets/local-image/asset-1") {
      return new Response(new Blob(["png"], { type: "image/png" }), {
        status: 200,
        headers: { "Content-Type": "image/png" },
      });
    }
    return jsonResponse({});
  });

  render(<App />);

  expect(await screen.findByText("see attached")).toBeInTheDocument();
  const image = await screen.findByRole("img", { name: "codex-clipboard.png" });
  expect(image).toHaveAttribute("src", "blob:codex-image");
  expect(globalThis.fetch).toHaveBeenCalledWith(
    "http://bridge.local/api/assets/local-image/asset-1",
    expect.objectContaining({
      headers: { Authorization: "Bearer session-1" },
    }),
  );
});
```

Add this failure-state test:

```ts
it("shows_attachment_failure_when_image_proxy_rejects_asset", async () => {
  stubObjectUrls();
  saveActiveSession();
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = String(input);
    if (url === "http://bridge.local/api/health") {
      return jsonResponse({ status: "ok", connectionState: "writable" });
    }
    if (url === "http://bridge.local/api/sessions") {
      return jsonResponse([
        sessionSnapshot({ threadId: "thread-image", title: "Image thread", preview: "Has image" }),
      ]);
    }
    if (url === "http://bridge.local/api/sessions/thread-image/events") {
      return jsonResponse([
        sessionEvent({
          id: "event-image",
          threadId: "thread-image",
          payload: {
            role: "user",
            text: "see attached",
            attachments: [
              {
                type: "image",
                src: "/api/assets/local-image/missing",
                name: "missing.png",
              },
            ],
          },
        }),
      ]);
    }
    if (url === "http://bridge.local/api/assets/local-image/missing") {
      return jsonResponse({ error: "asset not found" }, 404);
    }
    return jsonResponse({});
  });

  render(<App />);

  expect(await screen.findByText("Image unavailable: missing.png")).toBeInTheDocument();
});
```

- [ ] **Step 2: Run the targeted attachment tests and confirm failure**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx -t "image_attachments|attachment_failure"
```

Expected: both tests fail because event rows only render `payloadText` and there is no asset blob fetch API.

- [ ] **Step 3: Add protocol attachment types**

In `apps/mobile-pwa/src/protocol.ts`, add after `SessionEvent`:

```ts
export interface ImageAttachment {
  type: "image";
  src: string;
  name: string;
}

export interface MessagePayload {
  role?: string;
  text?: string;
  pending?: boolean;
  attachments?: ImageAttachment[];
}
```

- [ ] **Step 4: Add authenticated asset fetch API**

In `apps/mobile-pwa/src/api.ts`, add this exported function after `listSessionEvents`:

```ts
export async function fetchAssetBlob(
  bridgeUrl: string,
  sessionToken: string,
  src: string,
): Promise<Blob> {
  const response = await fetch(apiUrl(bridgeUrl, src), {
    headers: { Authorization: `Bearer ${sessionToken}` },
  });

  if (!response.ok) {
    throw new ApiError(response.status, `Asset request failed with ${response.status}`);
  }

  const contentType = response.headers.get("Content-Type") || "";
  if (!contentType.startsWith("image/")) {
    throw new ApiError(response.status, "Asset response is not an image");
  }

  return response.blob();
}
```

- [ ] **Step 5: Render attachment thumbnails**

In `apps/mobile-pwa/src/App.tsx`, add `fetchAssetBlob` to the existing `./api` import:

```ts
  fetchAssetBlob,
```

Add `ImageAttachment` to the existing `./protocol` type import:

```ts
  ImageAttachment,
```

Pass `deviceSession` into `SessionDetail`:

```tsx
<SessionDetail
  approvals={selectedApprovals}
  assetSession={deviceSession}
  events={selectedEvents}
  session={selectedSession}
/>
```

Update `SessionDetail` props:

```ts
function SessionDetail({
  approvals: sessionApprovals,
  assetSession,
  events: sessionEvents,
  session,
}: {
  approvals: ApprovalRequest[];
  assetSession: DeviceSession | null;
  events: SessionEvent[];
  session: SessionSnapshot | null;
}) {
```

Replace the event row map with:

```tsx
{sessionEvents.map((event) => (
  <EventRow assetSession={assetSession} event={event} key={event.id} />
))}
```

Add these components before `Composer`:

```tsx
function EventRow({
  assetSession,
  event,
}: {
  assetSession: DeviceSession | null;
  event: SessionEvent;
}) {
  const attachments = payloadImageAttachments(event.payload);

  return (
    <article className="event-row">
      <span className="event-icon" aria-hidden="true">
        {event.type === "tool_call" ? <TerminalSquare size={14} /> : <Clock3 size={14} />}
      </span>
      <div className="event-content">
        <p>{event.type.replace("_", " ")}</p>
        <span>{payloadText(event.payload)}</span>
        {attachments.length > 0 ? (
          <div className="attachment-list" aria-label="Image attachments">
            {attachments.map((attachment) => (
              <AttachmentImage
                attachment={attachment}
                assetSession={assetSession}
                key={`${event.id}:${attachment.src}`}
              />
            ))}
          </div>
        ) : null}
      </div>
    </article>
  );
}

function AttachmentImage({
  assetSession,
  attachment,
}: {
  assetSession: DeviceSession | null;
  attachment: ImageAttachment;
}) {
  const [objectUrl, setObjectUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!assetSession) {
      setFailed(true);
      return;
    }

    let active = true;
    let nextObjectUrl: string | null = null;

    async function loadImage() {
      try {
        const blob = await fetchAssetBlob(
          assetSession.bridgeUrl,
          assetSession.sessionToken,
          attachment.src,
        );
        if (!active) {
          return;
        }
        nextObjectUrl = URL.createObjectURL(blob);
        setObjectUrl(nextObjectUrl);
      } catch {
        if (active) {
          setFailed(true);
        }
      }
    }

    void loadImage();

    return () => {
      active = false;
      if (nextObjectUrl) {
        URL.revokeObjectURL(nextObjectUrl);
      }
    };
  }, [assetSession, attachment.src]);

  if (failed) {
    return <span className="attachment-error">Image unavailable: {attachment.name}</span>;
  }

  if (!objectUrl) {
    return <span className="attachment-loading">Loading image: {attachment.name}</span>;
  }

  return <img className="attachment-image" src={objectUrl} alt={attachment.name} />;
}
```

Add this parser near `payloadText`:

```ts
function payloadImageAttachments(payload: SessionEvent["payload"]): ImageAttachment[] {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return [];
  }
  const attachments = payload.attachments;
  if (!Array.isArray(attachments)) {
    return [];
  }

  return attachments.filter((attachment): attachment is ImageAttachment => {
    return (
      Boolean(attachment) &&
      typeof attachment === "object" &&
      !Array.isArray(attachment) &&
      attachment.type === "image" &&
      typeof attachment.src === "string" &&
      typeof attachment.name === "string"
    );
  });
}
```

- [ ] **Step 6: Add attachment styles**

In `apps/mobile-pwa/src/styles.css`, add after `.event-row span`:

```css
.event-content {
  min-width: 0;
}

.attachment-list {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-top: 8px;
}

.attachment-image {
  width: min(220px, 100%);
  max-height: 180px;
  border: 1px solid var(--border);
  border-radius: 7px;
  background: var(--surface-subtle);
  object-fit: contain;
}

.attachment-loading,
.attachment-error {
  display: inline-flex;
  min-height: 28px;
  align-items: center;
  padding: 0 8px;
  border: 1px solid var(--border);
  border-radius: 7px;
  background: var(--surface-subtle);
  color: var(--text-muted);
  font-size: 0.76rem;
}
```

- [ ] **Step 7: Run the targeted attachment tests and confirm pass**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run App.test.tsx -t "image_attachments|attachment_failure"
```

Expected: both targeted tests pass.

- [ ] **Step 8: Commit Task 3**

```bash
git add apps/mobile-pwa/src/api.ts apps/mobile-pwa/src/protocol.ts apps/mobile-pwa/src/App.tsx apps/mobile-pwa/src/App.test.tsx apps/mobile-pwa/src/styles.css
git commit -m "feat: render pwa image attachments"
```

---

### Task 4: Normalize Codex localImage Attachments

**Files:**
- Modify: `crates/bridge-core/src/normalizer.rs`

- [ ] **Step 1: Write failing normalizer test**

In `crates/bridge-core/src/normalizer.rs`, add this test:

```rust
#[test]
fn normalizes_local_image_parts_to_internal_attachments() {
    let turns = vec![CodexTurn {
        id: Some("turn-image".to_string()),
        thread_id: Some("thread-1".to_string()),
        created_at: Some(1_725_000_000_000),
        updated_at: None,
        raw: json!({
            "items": [
                {
                    "id": "item-1",
                    "type": "userMessage",
                    "content": [
                        { "type": "input_text", "text": "look at this" },
                        {
                            "type": "localImage",
                            "path": "/var/folders/codex-clipboard.png",
                            "detail": null
                        }
                    ]
                }
            ]
        }),
    }];

    let events = Normalizer::events_from_turns("thread-1", &turns);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].payload["text"], json!("look at this"));
    assert_eq!(
        events[0].payload["attachments"],
        json!([
            {
                "type": "image",
                "path": "/var/folders/codex-clipboard.png",
                "name": "codex-clipboard.png"
            }
        ])
    );
}
```

- [ ] **Step 2: Run the normalizer test and confirm failure**

Run:

```bash
cargo test -p bridge-core normalizes_local_image_parts_to_internal_attachments -- --nocapture
```

Expected: the test fails because payloads do not include `attachments`.

- [ ] **Step 3: Add local image extraction**

In `crates/bridge-core/src/normalizer.rs`, add `Path` to imports:

```rust
use std::path::Path;
```

Add this helper below `text_from_value`:

```rust
fn image_attachments_from_value(value: &Value) -> Vec<Value> {
    let mut attachments = Vec::new();
    collect_image_attachments(value, 0, &mut attachments);
    attachments
}

fn collect_image_attachments(value: &Value, depth: usize, attachments: &mut Vec<Value>) {
    if depth > 6 {
        return;
    }

    match value {
        Value::Array(items) => {
            for item in items {
                collect_image_attachments(item, depth + 1, attachments);
            }
        }
        Value::Object(object) => {
            let item_type = object
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if item_type == "localimage" {
                if let Some(path) = object.get("path").and_then(Value::as_str) {
                    attachments.push(json!({
                        "type": "image",
                        "path": path,
                        "name": file_name_from_path(path),
                    }));
                }
                return;
            }

            for child in object.values() {
                collect_image_attachments(child, depth + 1, attachments);
            }
        }
        _ => {}
    }
}

fn file_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("image")
        .to_string()
}
```

In `event_from_item`, replace the payload construction with:

```rust
let attachments = image_attachments_from_value(item);
let payload = if attachments.is_empty() {
    json!({
        "role": role,
        "text": text,
        "raw": item,
    })
} else {
    json!({
        "role": role,
        "text": text,
        "attachments": attachments,
        "raw": item,
    })
};

SessionEvent {
    id,
    thread_id: thread_id.to_string(),
    event_type,
    payload,
    created_at,
}
```

- [ ] **Step 4: Run normalizer tests and confirm pass**

Run:

```bash
cargo test -p bridge-core normalizer -- --nocapture
```

Expected: all normalizer tests pass.

- [ ] **Step 5: Commit Task 4**

```bash
git add crates/bridge-core/src/normalizer.rs
git commit -m "feat: normalize local image attachments"
```

---

### Task 5: Protected Local Image Proxy

**Files:**
- Modify: `crates/bridge-core/src/lib.rs`
- Create: `crates/bridge-core/src/local_assets.rs`
- Modify: `crates/bridge-core/src/http_api.rs`

- [ ] **Step 1: Write failing local asset registry tests**

Create `crates/bridge-core/src/local_assets.rs` with these tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn registry_reuses_token_for_same_path() {
        let mut registry = LocalAssetRegistry::default();
        let path = PathBuf::from("/var/folders/codex-clipboard.png");

        let first = registry.register_image(path.clone());
        let second = registry.register_image(path.clone());

        assert_eq!(first, second);
        assert_eq!(registry.path_for(&first), Some(path));
    }

    #[test]
    fn registry_returns_none_for_unknown_token() {
        let registry = LocalAssetRegistry::default();

        assert_eq!(registry.path_for("missing"), None);
    }
}
```

- [ ] **Step 2: Run registry tests and confirm failure**

Run:

```bash
cargo test -p bridge-core local_assets -- --nocapture
```

Expected: compilation fails because `LocalAssetRegistry` does not exist and the module is not exported.

- [ ] **Step 3: Implement local asset registry**

In `crates/bridge-core/src/lib.rs`, add:

```rust
pub mod local_assets;
```

In `crates/bridge-core/src/local_assets.rs`, add:

```rust
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use uuid::Uuid;

#[derive(Default)]
pub struct LocalAssetRegistry {
    by_path: HashMap<PathBuf, String>,
    by_token: HashMap<String, PathBuf>,
}

impl LocalAssetRegistry {
    pub fn register_image(&mut self, path: impl AsRef<Path>) -> String {
        let path = path.as_ref().to_path_buf();
        if let Some(token) = self.by_path.get(&path) {
            return token.clone();
        }

        let token = Uuid::new_v4().to_string();
        self.by_path.insert(path.clone(), token.clone());
        self.by_token.insert(token.clone(), path);
        token
    }

    pub fn path_for(&self, token: &str) -> Option<PathBuf> {
        self.by_token.get(token).cloned()
    }
}
```

- [ ] **Step 4: Run registry tests and confirm pass**

Run:

```bash
cargo test -p bridge-core local_assets -- --nocapture
```

Expected: registry tests pass.

- [ ] **Step 5: Write failing HTTP asset proxy tests**

In `crates/bridge-core/src/http_api.rs`, add `local_assets::LocalAssetRegistry` to the crate imports:

```rust
local_assets::LocalAssetRegistry,
```

Add these tests near the other HTTP API tests:

```rust
#[tokio::test]
async fn paired_device_receives_scrubbed_image_asset_url_and_can_fetch_image() {
    let (dir, state) = test_state();
    let image_path = dir.path().join("codex-clipboard.png");
    tokio::fs::write(&image_path, b"png-bytes")
        .await
        .expect("image fixture writes");
    let session_token = pair_device(&state).await;
    let adapter = Arc::new(RecordingAdapter::with_turns(
        "thread-image",
        vec![CodexTurn {
            id: Some("turn-image".to_string()),
            thread_id: Some("thread-image".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "items": [
                    {
                        "id": "item-1",
                        "type": "userMessage",
                        "content": [
                            { "type": "input_text", "text": "see image" },
                            { "type": "localImage", "path": image_path.to_string_lossy() }
                        ]
                    }
                ]
            }),
        }],
    ));
    let app = build_router(state.with_codex_adapter(adapter));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/sessions/thread-image/events")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_json(response).await;
    let attachment = &body[0]["payload"]["attachments"][0];
    assert_eq!(attachment["type"], json!("image"));
    assert_eq!(attachment["name"], json!("codex-clipboard.png"));
    assert!(attachment.get("path").is_none());
    let src = attachment["src"].as_str().expect("src is present");
    assert!(src.starts_with("/api/assets/local-image/"));

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(src)
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png",
    );
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("image body reads");
    assert_eq!(&bytes[..], b"png-bytes");
}
```

Add:

```rust
#[tokio::test]
async fn local_image_asset_route_rejects_missing_token() {
    let (_dir, state) = test_state();
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/assets/local-image/missing")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

Add:

```rust
#[tokio::test]
async fn unregistered_local_image_asset_returns_not_found() {
    let (_dir, state) = test_state();
    let session_token = pair_device(&state).await;
    let app = build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/assets/local-image/missing")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
```

Add:

```rust
#[tokio::test]
async fn non_image_local_asset_returns_unsupported_media_type() {
    let (dir, state) = test_state();
    let text_path = dir.path().join("not-image.txt");
    tokio::fs::write(&text_path, b"not image")
        .await
        .expect("text fixture writes");
    let session_token = pair_device(&state).await;
    let adapter = Arc::new(RecordingAdapter::with_turns(
        "thread-image",
        vec![CodexTurn {
            id: Some("turn-image".to_string()),
            thread_id: Some("thread-image".to_string()),
            created_at: Some(1_725_000_000_000),
            updated_at: None,
            raw: json!({
                "items": [
                    {
                        "id": "item-1",
                        "type": "userMessage",
                        "content": [
                            { "type": "localImage", "path": text_path.to_string_lossy() }
                        ]
                    }
                ]
            }),
        }],
    ));
    let app = build_router(state.with_codex_adapter(adapter));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/api/sessions/thread-image/events")
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");
    let body = response_json(response).await;
    let src = body[0]["payload"]["attachments"][0]["src"]
        .as_str()
        .expect("src is present");

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(src)
                .header(header::AUTHORIZATION, format!("Bearer {session_token}"))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
```

In the `RecordingAdapter` impl block, add:

```rust
fn with_turns(thread_id: &str, turns: Vec<CodexTurn>) -> Self {
    let mut map = StdHashMap::new();
    map.insert(thread_id.to_string(), turns);
    Self {
        turns: Arc::new(StdMutex::new(map)),
        ..Self::default()
    }
}
```

- [ ] **Step 6: Run HTTP asset tests and confirm failure**

Run:

```bash
cargo test -p bridge-core local_image_asset -- --nocapture
```

Expected: tests fail because `AppState` has no local asset registry and no `/api/assets/local-image/:asset_token` route.

- [ ] **Step 7: Add asset registry to AppState and route**

In `crates/bridge-core/src/http_api.rs`, extend imports:

```rust
use std::{
    collections::{HashMap, VecDeque},
    io,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::Arc,
};
```

Add `local_assets` to `AppState`:

```rust
local_assets: Arc<Mutex<LocalAssetRegistry>>,
```

Initialize it in `AppState::new`:

```rust
local_assets: Arc::new(Mutex::new(LocalAssetRegistry::default())),
```

Add the route inside `authenticated_routes`:

```rust
.route("/api/assets/local-image/:asset_token", get(get_local_image_asset))
```

- [ ] **Step 8: Scrub internal image paths into authenticated asset URLs**

In `crates/bridge-core/src/http_api.rs`, add:

```rust
impl AppState {
    async fn register_local_assets_for_event(&self, mut event: SessionEvent) -> SessionEvent {
        if let Some(attachments) = event
            .payload
            .get_mut("attachments")
            .and_then(serde_json::Value::as_array_mut)
        {
            let mut registry = self.local_assets.lock().await;
            for attachment in attachments {
                let Some(object) = attachment.as_object_mut() else {
                    continue;
                };
                let is_image = object
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    == Some("image");
                let path = object
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .map(PathBuf::from);
                if !is_image {
                    continue;
                }
                if let Some(path) = path {
                    let token = registry.register_image(&path);
                    object.remove("path");
                    object.insert(
                        "src".to_string(),
                        json!(format!("/api/assets/local-image/{token}")),
                    );
                }
            }
        }
        event
    }
}
```

In `list_session_events`, replace the adapter branch with:

```rust
if let Some(adapter) = state.codex_adapter.as_ref() {
    let turns = adapter.list_turns(&thread_id).await?;
    let mut events = Vec::new();
    for event in Normalizer::events_from_turns(&thread_id, &turns) {
        let event = state.register_local_assets_for_event(event).await;
        state.record_session_event(event.clone()).await;
        events.push(event);
    }
    return Ok(Json(events));
}
```

- [ ] **Step 9: Implement image proxy response**

In `crates/bridge-core/src/http_api.rs`, add:

```rust
async fn get_local_image_asset(
    State(state): State<AppState>,
    Path(asset_token): Path<String>,
) -> Result<Response, ApiError> {
    let path = state
        .local_assets
        .lock()
        .await
        .path_for(&asset_token)
        .ok_or(ApiError::NotFound("asset not found"))?;
    let content_type =
        image_content_type(&path).ok_or(ApiError::UnsupportedMediaType("asset is not an image"))?;
    let bytes = tokio::fs::read(&path).await.map_err(|error| match error.kind() {
        io::ErrorKind::PermissionDenied => ApiError::Forbidden("asset is not readable"),
        _ => ApiError::NotFound("asset not found"),
    })?;

    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

fn image_content_type(path: &FsPath) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}
```

Add `header` to the top-level `axum::http` import:

```rust
http::{HeaderMap, Request, StatusCode, header},
```

Extend `ApiError`:

```rust
Forbidden(&'static str),
NotFound(&'static str),
UnsupportedMediaType(&'static str),
```

Extend `IntoResponse for ApiError`:

```rust
Self::Forbidden(message) => (StatusCode::FORBIDDEN, message.to_string()),
Self::NotFound(message) => (StatusCode::NOT_FOUND, message.to_string()),
Self::UnsupportedMediaType(message) => {
    (StatusCode::UNSUPPORTED_MEDIA_TYPE, message.to_string())
}
```

- [ ] **Step 10: Run HTTP asset tests and confirm pass**

Run:

```bash
cargo test -p bridge-core local_image_asset -- --nocapture
```

Expected: asset proxy tests pass.

- [ ] **Step 11: Run broader backend tests**

Run:

```bash
cargo test -p bridge-core -- --nocapture
```

Expected: all `bridge-core` tests pass.

- [ ] **Step 12: Commit Task 5**

```bash
git add crates/bridge-core/src/lib.rs crates/bridge-core/src/local_assets.rs crates/bridge-core/src/http_api.rs
git commit -m "feat: proxy local image assets"
```

---

### Task 6: Full Verification And Manual PWA Smoke Check

**Files:**
- Modify only if verification reveals a defect in Task 1-5 files.

- [ ] **Step 1: Run all PWA tests**

Run:

```bash
cd apps/mobile-pwa && npm test -- --run
```

Expected: all Vitest tests pass.

- [ ] **Step 2: Build the PWA**

Run:

```bash
cd apps/mobile-pwa && npm run build
```

Expected: TypeScript and Vite build pass.

- [ ] **Step 3: Run all Rust tests**

Run:

```bash
cargo test --workspace -- --nocapture
```

Expected: all Rust workspace tests pass.

- [ ] **Step 4: Start the sidecar and smoke check in browser**

Run the existing sidecar command used by this repo. If the previous terminal session is still running, reuse its URL. Otherwise start:

```bash
cargo run -p bridge-sidecar -- --host 0.0.0.0 --port 57324
```

Expected: terminal prints a LAN pairing URL for the current Wi-Fi IP. Open the PWA and verify:

- Sessions panel remains visible while the event stream scrolls.
- Event stream shows older messages above newer messages.
- When the stream is at bottom, new Codex output stays visible at bottom.
- When scrolled upward, new Codex output does not jump the view.
- A message with a `localImage` attachment renders a thumbnail instead of only showing the file path text.

- [ ] **Step 5: Final commit if Task 6 required fixes**

If Task 6 changed files, commit the fixes:

```bash
git add apps/mobile-pwa/src crates/bridge-core/src
git commit -m "fix: polish pwa thread ui verification"
```

If Task 6 made no file changes, do not create an empty commit.

---

## Self-Review

- Spec coverage: Task 1 covers independent Sessions/event scrolling and old-to-new ordering. Task 2 covers bottom-stick behavior. Task 3 covers frontend image thumbnail rendering and failure state. Task 4 covers normalizer extraction from `localImage`. Task 5 covers token registry, path scrubbing, protected image proxy, 401/404/415 behavior. Task 6 covers final tests and manual smoke check.
- Placeholder scan: no task contains incomplete sections or vague test instructions; each code-changing step names concrete files and snippets.
- Type consistency: frontend attachment shape is `ImageAttachment { type, src, name }`; backend internal normalizer shape uses `{ type, path, name }` and HTTP API replaces `path` with `src` before returning events. The route path `/api/assets/local-image/:asset_token` matches frontend `fetchAssetBlob` and test expectations.
