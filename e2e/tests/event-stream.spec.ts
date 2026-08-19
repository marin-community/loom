import type { Page, Request } from "@playwright/test";
import { test, expect } from "../fixtures/weaver";
import type { Session, WeaverFixture } from "../fixtures/weaver";
import { join } from "path";

// The page's live streams all ride one connection.
//
// Browsers cap HTTP/1.1 at 6 connections per origin and an EventSource holds one
// for its whole life. The UI used to open three (fleet layout, the session's
// event feed, its ACP chat deltas), so two tabs exhausted the pool and every
// `fetch()` after that hung with no error and nothing in the server log. These
// specs assert the property that prevents it — not the plumbing, the count.

const FAKE_AGENT = join(
  __dirname,
  "..",
  "..",
  "crates",
  "loom",
  "tests",
  "fixtures",
  "fake-acp-agent.mjs",
);
const HEADERS = { "content-type": "application/json" };
const SKIP_MSG = "server does not launch acp sessions over REST here";

/** Any request that holds a connection open for its lifetime. */
function isStream(url: string): boolean {
  const path = new URL(url).pathname;
  return [
    "/api/events/stream",
    "/api/logs/stream",
    "/api/sessions/chat/stream",
    "/api/sessions/events/stream",
    "/api/session_layout/events",
  ].includes(path);
}

/**
 * Track streaming requests that are still open. An SSE request never
 * "finishes", so anything left in this set is holding one of the six slots.
 */
function trackStreams(page: Page): Set<Request> {
  const inflight = new Set<Request>();
  page.on("request", (r) => {
    if (isStream(r.url())) inflight.add(r);
  });
  page.on("requestfinished", (r) => inflight.delete(r));
  page.on("requestfailed", (r) => inflight.delete(r));
  return inflight;
}

async function launchAcpSession(
  weaver: WeaverFixture,
  opts: { goal: string; name: string },
): Promise<Session | null> {
  const res = await fetch(`${weaver.baseUrl}/api/agents/custom/create`, {
    method: "POST",
    headers: HEADERS,
    body: JSON.stringify({
      name: "acp-fake",
      label: "ACP fake",
      setup: "",
      launch: `node ${FAKE_AGENT}`,
      resume: "",
      reports_status: false,
      protocol: "acp",
    }),
  });
  if (!res.ok && res.status !== 409)
    throw new Error(`defining the fake agent failed: ${await res.text()}`);

  const created = await fetch(`${weaver.baseUrl}/api/sessions/launch`, {
    method: "POST",
    headers: HEADERS,
    body: JSON.stringify({
      goal: opts.goal,
      cwd: weaver.repoPath,
      agent: "acp-fake",
      name: opts.name,
      protocol: "acp",
      mode: "default",
    }),
  });
  if (!created.ok) return null;
  const s = (await created.json()) as Session & { protocol?: string };
  return s.protocol === "acp" ? s : null;
}

test.describe("multiplexed event stream", () => {
  test("a session page holds exactly one streaming connection", async ({
    page,
    weaver,
  }) => {
    const inflight = trackStreams(page);
    const s = await launchAcpSession(weaver, {
      goal: "say:Streaming over one socket.",
      name: "mux-one-conn",
    });
    test.skip(s === null, SKIP_MSG);

    await page.goto(`${weaver.baseUrl}/s/${s!.id}`);
    // The conversation rendering means layout, session events, and chat deltas
    // are all live — the three streams this used to cost.
    await expect(page.getByTestId("acp-conversation")).toBeVisible();
    await expect(page.getByText("Streaming over one socket.")).toBeVisible();

    const open = [...inflight];
    expect(
      open.map((r) => r.url()),
      "the whole page streams over a single connection",
    ).toHaveLength(1);

    // ...and it is the multiplexed route, carrying both per-session topics.
    const url = open[0].url();
    expect(url).toContain("/api/events/stream?topics=");
    const topics = decodeURIComponent(
      new URL(url).searchParams.get("topics") ?? "",
    ).split(",");
    expect(topics).toContain("layout");
    expect(topics).toContain(`session:${s!.id}`);
    expect(topics).toContain(`chat:${s!.id}`);
  });

  test("navigating between sessions does not accumulate connections", async ({
    page,
    weaver,
  }) => {
    const inflight = trackStreams(page);
    const first = await launchAcpSession(weaver, {
      goal: "say:First session.",
      name: "mux-nav-first",
    });
    test.skip(first === null, SKIP_MSG);
    const second = await launchAcpSession(weaver, {
      goal: "say:Second session.",
      name: "mux-nav-second",
    });
    test.skip(second === null, SKIP_MSG);

    await page.goto(`${weaver.baseUrl}/s/${first!.id}`);
    await expect(page.getByText("First session.")).toBeVisible();

    // Round-trip into the other session and back. Each hop re-subscribes several
    // components; the shared stream must reconnect, not stack up.
    await page.goto(`${weaver.baseUrl}/s/${second!.id}`);
    await expect(page.getByText("Second session.")).toBeVisible();
    await page.goto(`${weaver.baseUrl}/s/${first!.id}`);
    await expect(page.getByText("First session.")).toBeVisible();

    expect(
      [...inflight].map((r) => r.url()),
      "navigation replaces the connection instead of adding one",
    ).toHaveLength(1);
  });
});
