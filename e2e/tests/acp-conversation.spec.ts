import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures/weaver';
import type { Session, WeaverFixture } from '../fixtures/weaver';
import { join } from 'path';
import { existsSync, readFileSync, unlinkSync, writeFileSync } from 'fs';

// The Conversation surface for an ACP session (`protocol='acp'`): a live,
// journal-backed transcript rendered as typeset dialogue — serif prose for the
// humans and the agent, the machine's apparatus (tool calls, diffs, command
// output) set as mono blocks between the prose.
//
// These specs drive a *fake ACP agent* — a scripted ~200-line node adapter
// (crates/loom/tests/fixtures/fake-acp-agent.mjs) that speaks the real ACP wire
// format. A prompt's text is a `|`-separated micro-script the fake interprets:
//
//   say:TEXT             two agent_message_chunks that consolidate to TEXT
//   think:TEXT           an agent_thought_chunk
//   tool:KIND[:TITLE]    a tool_call (in_progress → completed); `edit` carries a diff
//   image[:TITLE]        a read tool that carries an ACP image content block
//   image-file:PATH      a read tool that links to a local image
//   plan                 a plan update
//   usage:USED:SIZE      a usage_update
//   wait:MS              sleep (keeps the turn live — for queue/interrupt tests)
//   permission:NAME      a session/request_permission that blocks the turn
//   resources            echo the names of ACP resource_link blocks
//
// The ACP *lifecycle* backend (weaver #508 — the protocol axis on agents +
// sessions, create/adopt branching, `--mode`) is a parallel phase. Until it
// merges, the server can't launch an acp session over REST, so `launchAcpSession`
// returns null and every test self-skips with a clear message. The specs are
// written to activate unmodified once that phase lands — the only backend-shaped
// assumptions (how a custom agent declares `protocol`/`launch`, and the create
// body's `protocol`/`mode` fields) live in `launchAcpSession`.

const FAKE_AGENT = join(
  __dirname,
  '..',
  '..',
  'crates',
  'loom',
  'tests',
  'fixtures',
  'fake-acp-agent.mjs',
);
const HEADERS = { 'content-type': 'application/json' };
const SKIP_MSG =
  'ACP lifecycle backend (weaver #508) not merged: the server does not launch acp sessions over REST yet';

async function defineFakeAcpAgent(
  weaver: WeaverFixture,
  name: string,
  label: string,
  options: { summaryDelay?: number; steering?: boolean; nativeQueue?: boolean } = {},
): Promise<void> {
  const summaryEnv = options.summaryDelay
    ? `FAKE_ACP_SUMMARY_OUTPUT_B64= FAKE_ACP_SUMMARY_DELAY=${options.summaryDelay} `
    : '';
  const res = await fetch(`${weaver.baseUrl}/api/agents/custom/create`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({
      name,
      label,
      setup: '',
      launch: `${summaryEnv}${options.steering ? 'FAKE_ACP_STEERING=1 ' : ''}${options.nativeQueue ? 'FAKE_ACP_PROMPT_QUEUEING=1 ' : ''}node ${FAKE_AGENT}`,
      resume: '',
      reports_status: false,
      protocol: 'acp',
    }),
  });
  // Tests sharing a worker can race to define the common source adapter.
  if (!res.ok && res.status !== 409)
    throw new Error(`defining ${name} failed: ${await res.text()}`);
}

/** Define (idempotently) a custom agent that runs the fake ACP adapter over
 *  stdio, then create a session with it. Returns the session when the backend
 *  brought it up as `protocol='acp'`, else null (the acp axis isn't wired yet). */
async function launchAcpSession(
  weaver: WeaverFixture,
  opts: { goal: string; mode?: string; name?: string; steering?: boolean; nativeQueue?: boolean },
): Promise<Session | null> {
  // The fake adapter is launched by the custom agent's `launch` command. The
  // `protocol` axis marks it ACP rather than a PTY TUI (added by the lifecycle
  // phase; ignored by the current backend, which is exactly why the probe below
  // detects support).
  const agent = opts.nativeQueue
    ? 'acp-fake-native-queue'
    : opts.steering
      ? 'acp-fake-steering'
      : 'acp-fake';
  await defineFakeAcpAgent(weaver, agent, 'ACP fake', {
    steering: opts.steering,
    nativeQueue: opts.nativeQueue,
  });

  const res = await fetch(`${weaver.baseUrl}/api/sessions/launch`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({
      goal: opts.goal,
      cwd: weaver.repoPath,
      agent,
      name: opts.name ?? 'acp',
      protocol: 'acp',
      mode: opts.mode ?? 'default',
    }),
  });
  if (!res.ok) return null;
  const s = (await res.json()) as Session & { protocol?: string };
  if (s.protocol !== 'acp') return null;
  return s;
}

/** Launch + open the conversation, or skip the test when acp isn't supported. */
async function openAcp(
  page: Page,
  weaver: WeaverFixture,
  opts: { goal: string; mode?: string; name?: string; steering?: boolean; nativeQueue?: boolean },
): Promise<Session> {
  const s = await launchAcpSession(weaver, opts);
  test.skip(s === null, SKIP_MSG);
  await page.setViewportSize({ width: 1360, height: 900 });
  await page.goto(`${weaver.baseUrl}/s/${s!.id}`);
  await expect(page.getByTestId('acp-conversation')).toBeVisible();
  return s!;
}

/** Exercise queued feedback from another client through the same API. */
async function queueAcpPrompt(
  weaver: WeaverFixture,
  session: Session,
  text: string,
): Promise<void> {
  const res = await fetch(`${weaver.baseUrl}/api/sessions/prompt/create`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({ text, session: session.id }),
  });
  if (!res.ok) throw new Error(`queueing ACP prompt failed: ${await res.text()}`);
}

/** Emulate the display-toggle reset some browsers (notably Safari) apply to a
 *  scroll container hidden by `v-show`. The real hidden ancestor is the work-area
 *  wrapper, not the scroller's direct parent: lay that pane out invisibly, zero
 *  the offset, let the scroll event dispatch while the app still treats the pane
 *  as inactive, then hide it again. */
async function dropHiddenScrollOffset(conv: ReturnType<Page['getByTestId']>) {
  await conv.evaluate(async (el) => {
    let pane = el.parentElement as HTMLElement | null;
    while (pane && getComputedStyle(pane).display !== 'none') pane = pane.parentElement;
    if (!pane) throw new Error('conversation pane is not hidden');
    const previousDisplay = pane.style.display;
    pane.style.display = 'block';
    pane.style.visibility = 'hidden';
    el.scrollTop = 0;
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
    pane.style.visibility = '';
    pane.style.display = previousDisplay;
  });
}

function footDistance(conv: ReturnType<Page['getByTestId']>) {
  return conv.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight);
}

async function holdNextChatSnapshot(page: Page): Promise<{
  read: Promise<void>;
  release: () => void;
}> {
  let releaseSnapshot!: () => void;
  let snapshotRead!: () => void;
  const release = new Promise<void>((resolve) => (releaseSnapshot = resolve));
  const read = new Promise<void>((resolve) => (snapshotRead = resolve));
  await page.route(
    '**/api/sessions/chat',
    async (route) => {
      const response = await route.fetch();
      snapshotRead();
      await release;
      await route.fulfill({ response });
    },
    { times: 1 },
  );
  return { read, release: releaseSnapshot };
}

test.describe('acp conversation', () => {
  test('stored history renders while the live stream is still connecting', async ({
    page,
    weaver,
  }) => {
    const session = await openAcp(page, weaver, {
      goal: 'say:stored reply',
      name: 'history-before-stream',
    });
    await expect(page.getByText('stored reply', { exact: true })).toBeVisible();
    let releaseStream!: () => void;
    const streamGate = new Promise<void>((resolve) => (releaseStream = resolve));
    await page.route('**/api/events/stream?**', async (route) => {
      await streamGate;
      await route.continue();
    });
    // Pause timers so a delayed fallback cannot masquerade as an immediate read.
    await page.clock.install();
    await page.clock.pauseAt(new Date());
    try {
      await page.reload();
      await expect(page.getByText('stored reply', { exact: true })).toBeVisible();
      await expect(page.getByText('Loading conversation…', { exact: true })).toBeHidden();
      await queueAcpPrompt(weaver, session, 'say:reply during the stream gap');
      await expect
        .poll(async () => {
          const response = await fetch(`${weaver.baseUrl}/api/sessions/chat`, {
            method: 'POST',
            headers: HEADERS,
            body: JSON.stringify({ session: session.id }),
          });
          const chat = await response.json();
          return chat.blocks.some(
            (block: { kind: string; payload: { text?: string } }) =>
              block.kind === 'agent_message' &&
              block.payload.text === 'reply during the stream gap',
          );
        })
        .toBe(true);
    } finally {
      releaseStream();
      await page.clock.resume();
    }
    await expect(page.getByText('reply during the stream gap', { exact: true })).toBeVisible();
  });

  test('reconciles a stream gap larger than the newest journal page', async ({
    page,
    weaver,
  }) => {
    const session = await launchAcpSession(weaver, {
      goal: 'say:ready',
      name: 'history-long-gap',
    });
    test.skip(session === null, SKIP_MSG);
    let releaseStream!: () => void;
    const streamGate = new Promise<void>(
      (resolve) => (releaseStream = resolve),
    );
    await page.route('**/api/events/stream?**', async (route) => {
      await streamGate;
      await route.continue();
    });
    let newestReads = 0;
    await page.route('**/api/sessions/chat', (route) => {
      const request = route.request().postDataJSON() as { before_seq?: number };
      const older = request.before_seq !== undefined;
      if (!older) newestReads++;
      const blocks = older
        ? [
            {
              turn: 8,
              seq: 0,
              kind: 'agent_message',
              payload: { text: 'known reply' },
            },
            {
              turn: 8,
              seq: 1,
              kind: 'agent_message',
              payload: { text: 'gap reply' },
            },
          ]
        : newestReads === 1
          ? [
              {
                turn: 8,
                seq: 0,
                kind: 'agent_message',
                payload: { text: 'known reply' },
              },
            ]
          : Array.from({ length: 200 }, (_, index) => ({
              turn: 8,
              seq: index + 2,
              kind: 'agent_message',
              payload: { text: `new reply ${index + 2}` },
            }));
      return route.fulfill({
        json: {
          blocks: blocks.map((block) => ({
            ...block,
            created_at: '2026-07-23T00:00:00Z',
          })),
          older_cursor: older
            ? null
            : newestReads === 1
              ? null
              : { turn: 8, seq: 2 },
          live_turn: null,
          effective_mode: null,
          pending_inputs: [],
          metadata: {
            commands: [],
            config_options: [],
            modes: [],
            steering_supported: false,
          },
        },
      });
    });
    try {
      await page.goto(`${weaver.baseUrl}/s/${session!.id}`);
      await expect(
        page.getByText('known reply', { exact: true }),
      ).toBeVisible();
    } finally {
      releaseStream();
    }
    await expect(page.getByTestId('acp-conversation')).toContainText('gap reply');
    await expect(page.getByTestId('acp-load-older')).toHaveCount(0);
  });

  test('a failed lag resync keeps history visible and offers retry', async ({
    page,
    weaver,
  }) => {
    await page.addInitScript(() => {
      const NativeEventSource = window.EventSource;
      window.EventSource = class extends NativeEventSource {
        constructor(url: string | URL, options?: EventSourceInit) {
          super(url, options);
          Object.defineProperty(window, '__testEventSource', {
            value: this,
            configurable: true,
          });
        }
      };
    });
    let failRefresh = false;
    let reads = 0;
    await page.route('**/api/sessions/chat', (route) => {
      reads++;
      return failRefresh
        ? route.fulfill({ status: 503, json: { error: 'refresh unavailable' } })
        : route.continue();
    });
    const session = await openAcp(page, weaver, {
      goal: 'say:history survives',
      name: 'history-resync-failure',
    });
    const reply = page.getByText('history survives', { exact: true });
    await expect(reply).toBeVisible();
    await expect.poll(() => reads).toBeGreaterThanOrEqual(2);
    failRefresh = true;
    await page.evaluate((id) => {
      const source = (window as Window & { __testEventSource?: EventSource })
        .__testEventSource;
      if (!source) throw new Error('event stream was not opened');
      source.dispatchEvent(
        new MessageEvent('message', {
          data: JSON.stringify({
            topic: `chat:${id}`,
            event: 'resync',
            data: { skipped: 1 },
          }),
        }),
      );
    }, session.id);
    const alert = page.getByRole('alert');
    await expect(alert).toContainText('refresh unavailable');
    await expect(reply).toBeVisible();
    failRefresh = false;
    await alert.getByRole('button', { name: 'Retry' }).click();
    await expect(alert).toHaveCount(0);
    await expect(reply).toBeVisible();
  });

  test('a stalled history read times out and can be retried', async ({ page, weaver }) => {
    await openAcp(page, weaver, { goal: 'say:retry recovered', name: 'history-timeout' });
    await expect(page.getByText('retry recovered', { exact: true })).toBeVisible();
    // Keep SSE disconnected so it cannot reconcile over the stalled read.
    await page.route('**/api/events/stream?**', (route) => route.abort());
    let releaseRead!: () => void;
    const readGate = new Promise<void>((resolve) => (releaseRead = resolve));
    let stalled = true;
    await page.route('**/api/sessions/chat', async (route) => {
      if (stalled) await readGate;
      await route.continue();
    });
    try {
      await page.reload();
      // AbortSignal.timeout uses the browser's active-time clock, not JS timers.
      await expect(page.getByRole('alert')).toContainText('timed out', { timeout: 20_000 });
      stalled = false;
      releaseRead();
      await page.getByRole('button', { name: 'Retry', exact: true }).click();
      await expect(page.getByText('retry recovered', { exact: true })).toBeVisible();
      await expect(page.getByRole('alert')).toHaveCount(0);
    } finally {
      releaseRead();
    }
  });

  test('an unfinished reply survives reloads and resumed tabs', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:running checks|tool:execute|say:the checks already passed|wait:30000',
      name: 'live-reply-replay',
    });
    const reply = page.getByTestId('acp-conversation').getByText('the checks already passed', {
      exact: true,
    });
    await expect(reply).toBeVisible();
    await expect(page.getByTestId('acp-working')).toBeVisible();

    await page.reload();
    await expect(reply).toBeVisible();
    await expect(page.getByTestId('acp-working')).toBeVisible();

    const reconnected = page.waitForRequest((request) =>
      request.url().includes('/api/events/stream?'),
    );
    await page.evaluate(() => window.dispatchEvent(new Event('focus')));
    await reconnected;
    await expect(reply).toBeVisible();
    await expect(reply).toHaveCount(1);
    await page.getByTestId('acp-composer-stop').click();
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await expect(reply).toBeVisible();
    await expect(reply).toHaveCount(1);
  });

  test('a turn end wins over an older chat snapshot', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'live-turn-snapshot',
    });
    await expect(page.getByText('ready', { exact: true })).toBeVisible();
    const snapshot = await holdNextChatSnapshot(page);
    await page.getByTestId('acp-composer-input').fill('say:completed reply|wait:30000');
    await page.getByTestId('acp-composer-send').click();
    await snapshot.read;
    try {
      await expect(page.getByTestId('acp-working')).toBeVisible();
      await page.getByTestId('acp-composer-stop').click();
      await expect(page.getByTestId('acp-working')).toBeHidden();
    } finally {
      snapshot.release();
    }
    await expect(page.getByText('completed reply', { exact: true })).toBeVisible();
    await expect(page.getByTestId('acp-composer-input')).toBeEnabled();
    await expect(page.getByTestId('acp-working')).toBeHidden();
  });

  test('a queue event wins over an older chat snapshot', async ({ page, weaver }) => {
    const session = await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'queue-snapshot',
    });
    await expect(page.getByText('ready', { exact: true })).toBeVisible();
    const snapshot = await holdNextChatSnapshot(page);
    await page.getByTestId('acp-composer-input').fill('wait:30000|say:unfinished');
    await page.getByTestId('acp-composer-send').click();
    await snapshot.read;
    try {
      await queueAcpPrompt(weaver, session, 'say:keep this queued');
      await expect(page.getByTestId('acp-pending')).toContainText('say:keep this queued');
    } finally {
      snapshot.release();
    }
    await expect(page.getByTestId('acp-composer-send')).toHaveText('Send');
    await expect(page.getByTestId('acp-pending')).toContainText('say:keep this queued', {
      timeout: 2000,
    });
  });

  test('session updates stay attributed and survive editing human feedback', async ({
    page,
    weaver,
  }) => {
    const session = await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'separate-inbox',
    });
    const source = await weaver.seedSession({
      goal: 'child work',
      name: 'update-source',
    });
    const input = page.getByTestId('acp-composer-input');
    await input.fill('wait:30000|say:unfinished');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible();
    await queueAcpPrompt(weaver, session, 'human feedback');
    await weaver.seedSessionUpdate(session, source, 'The child result is ready.');
    await page.reload();
    const update = page.getByTestId('acp-session-update');
    await expect(update).toHaveCount(1);
    await expect(update.locator('.acp-label')).toHaveText(`Update from loom session ${source.id}`);
    const timeout = update.getByTestId('acp-update-timeout');
    await expect(timeout).toHaveText(/Auto-send in \d+:\d{2}/);
    const initialTimeout = await timeout.textContent();
    await expect(timeout).not.toHaveText(initialTimeout!);
    await expect(update.getByTestId('acp-force-update')).toHaveText('Stop & send');
    await expect(update.getByRole('button', { name: 'Edit', exact: true })).toHaveCount(0);
    await expect(page.getByTestId('acp-pending').locator('.acp-label')).toHaveText('You');
    await page.getByTestId('acp-edit-queued').click();
    await expect(input).toHaveValue('human feedback');
    await expect(page.getByTestId('acp-pending')).toBeHidden();
    await expect(update).toContainText('The child result is ready.');
    await page.getByTestId('acp-composer-stop').click();
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await page.reload();
    await expect(update).toHaveCount(1);
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await expect(update.getByTestId('acp-update-timeout')).toHaveText('Waiting to start');
    await expect(update.getByTestId('acp-force-update')).toHaveText('Send now');
    await page.screenshot({
      path: test.info().outputPath('session-update-queue.png'),
    });
    await input.fill('unsent feedback');
    await update.getByTestId('acp-force-update').click();
    await expect(update).toHaveCount(0);
    const delivered = page
      .locator('.acp-speaker')
      .filter({ hasText: 'The child result is ready.' });
    await expect(delivered.locator('.acp-label')).toHaveText(
      `Update from loom session ${source.id}`,
    );
    await expect(input).toHaveValue('unsent feedback');
  });

  // Fails on main: the queued update is never cleared. https://github.com/marin-community/loom/issues/432
  test.fixme('stop and send forces a queued agent update during live work', async ({ page, weaver }) => {
    const session = await openAcp(page, weaver, {
      goal: 'wait:30000|say:unfinished work',
      name: 'force-agent-update',
    });
    const source = await weaver.seedSession({
      goal: 'child work',
      name: 'force-source',
    });
    await weaver.seedSessionUpdate(session, source, 'The latest review is ready.');
    await page.reload();
    const update = page.getByTestId('acp-session-update');
    await expect(update.getByTestId('acp-force-update')).toHaveText('Stop & send');
    await page.screenshot({
      path: test.info().outputPath('force-agent-update.png'),
    });
    await update.getByTestId('acp-force-update').click();
    await expect(update).toHaveCount(0);
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await expect(page.getByText('unfinished work', { exact: true })).toHaveCount(0);
    const delivered = page
      .locator('.acp-speaker')
      .filter({ hasText: 'The latest review is ready.' });
    await expect(delivered.locator('.acp-label')).toHaveText(
      `Update from loom session ${source.id}`,
    );
  });

  test('opens the first worktree shell from the keyboard', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:Ready for shell work.',
      name: 'acp-shell-keyboard',
    });

    await page.keyboard.press('2');
    await expect(page.getByTestId('acp-open-shell')).toBeVisible();
    await page.keyboard.press('n');
    await expect(page.getByRole('button', { name: 'Shell 1' })).toBeVisible();
    await expect(page.locator('.xterm')).toBeVisible();
  });

  // A chat opens at its newest exchange: the transcript scrolls to its foot on
  // load and stays there while the async markdown paint grows the content.
  test('opens scrolled to the foot of a long transcript', async ({ page, weaver }) => {
    // Interleave tool calls so each say stays its own speaker block (consecutive
    // message chunks would otherwise consolidate into one short message).
    const goal = Array.from(
      { length: 15 },
      (_, i) => `say:Paragraph ${i + 1} of a long transcript.|tool:read:Read file ${i + 1}`,
    ).join('|');
    await openAcp(page, weaver, { goal, name: 'acp-foot' });
    const conv = page.getByTestId('acp-conversation');
    // The turn has settled — its closing rule renders after the prose.
    await expect(page.getByTestId('acp-turn-rule')).toBeVisible({
      timeout: 20_000,
    });

    // The transcript genuinely overflows… (generous timeouts: markdown paints
    // asynchronously and can lag well behind the stream on a loaded machine)
    await expect
      .poll(() => conv.evaluate((el) => el.scrollHeight - el.clientHeight), {
        timeout: 30_000,
      })
      .toBeGreaterThan(300);
    // …and the view rests pinned at its foot, where the newest prose reads.
    await expect
      .poll(() => conv.evaluate((el) => el.scrollHeight - el.scrollTop - el.clientHeight), {
        timeout: 30_000,
      })
      .toBeLessThan(120);
  });

  test('renders the journaled transcript — user, agent, tool blocks', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:The route resolves against auth.base_url now.|tool:edit:Edit web.rs',
    });
    const conv = page.getByTestId('acp-conversation');

    // The agent's consolidated prose (two streamed chunks → one message).
    await expect(
      conv.getByText('The route resolves against auth.base_url now.', {
        exact: true,
      }),
    ).toBeVisible();
    // A lone edit is one disclosure: opening it reveals the inline diff preview.
    const head = page.getByTestId('acp-activity-head').filter({ hasText: 'Edit web.rs' });
    await expect(head).toBeVisible();
    await expect(page.getByTestId('acp-diff')).toBeHidden();
    await head.click();
    const diff = page.getByTestId('acp-diff');
    await expect(diff).toContainText('- old line');
    await expect(diff).toContainText('+ new line');
    await expect(diff).not.toContainText('unchanged');

    // The bounded preview can expand into a full-screen code viewer and closes
    // with Escape, returning focus to the expansion control.
    const expand = page.getByTestId('tool-content-expand');
    await expand.click();
    const viewer = page.getByTestId('tool-detail-dialog');
    await expect(viewer).toContainText('Edit web.rs');
    await expect(viewer).toContainText('new line');
    await page.keyboard.press('Escape');
    await expect(viewer).toBeHidden();
    await expect(expand).toBeFocused();
  });

  test('keeps viewed images and files visible in a collapsed tool run', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, {
      goal: 'tool:read:Read source|image:View Image /tmp/before.png|image:View Image /tmp/after.png|say:files inspected',
    });
    await expect(page.getByText('files inspected', { exact: true })).toBeVisible();

    const activity = page.getByTestId('acp-activity');
    await expect(activity.getByTestId('acp-activity-item')).toHaveCount(0);
    await expect(activity.getByTestId('tool-image-preview')).toHaveCount(2);
    const file = activity.getByTestId('tool-file-preview');
    await expect(file).toContainText('file.rs');
    await file.click();
    const viewer = page.getByTestId('tool-detail-dialog');
    await expect(viewer).toContainText('/w/file.rs');
    await expect(viewer.locator('pre')).toHaveText('done');
    await page.keyboard.press('Escape');
    await expect(file).toBeFocused();

    const preview = activity.getByTestId('tool-image-preview').first();
    await expect(preview.locator('img')).toHaveAttribute('src', /^data:image\/png;base64,/);
    await preview.focus();
    await page.keyboard.press('Enter');

    await expect(viewer).toContainText('View screenshot.png');
    await expect(viewer).toContainText('file:///tmp/screenshot.png');
    await expect(page.getByTestId('tool-detail-image')).toHaveAttribute(
      'src',
      /^data:image\/png;base64,/,
    );
    await page.keyboard.press('Escape');
    await expect(viewer).toBeHidden();
    await expect(preview).toBeFocused();

    await activity.getByTestId('acp-activity-head').click();
    await expect(activity.getByTestId('acp-activity-item')).toHaveCount(3);
    await expect(activity.getByTestId('tool-image-preview')).toHaveCount(2);

    await page.reload();
    await expect(page.getByTestId('tool-image-preview')).toHaveCount(2);
    await expect(page.getByTestId('tool-file-preview')).toHaveCount(1);
    await expect(activity.getByTestId('acp-activity-item')).toHaveCount(0);
    await page.setViewportSize({ width: 390, height: 844 });
    const lastImage = activity.getByTestId('tool-image-preview').last();
    await lastImage.focus();
    await expect(lastImage).toBeInViewport({ ratio: 0.95 });
    await page.screenshot({ path: test.info().outputPath('inline-previews-mobile.png') });
  });

  test('opens a solo image on a phone without expanding a tool row', async ({ page, weaver }) => {
    const path = join(weaver.repoPath, 'screenshot.png');
    const png = await page.evaluate(() => {
      const canvas = document.createElement('canvas');
      canvas.width = 1440;
      canvas.height = 1000;
      const context = canvas.getContext('2d')!;
      context.fillStyle = '#345b7d';
      context.fillRect(0, 0, canvas.width, canvas.height);
      return canvas.toDataURL('image/png').split(',')[1];
    });
    writeFileSync(path, Buffer.from(png, 'base64'));
    await openAcp(page, weaver, { goal: `image-file:${path}|say:inspected` });
    await expect(page.getByText('inspected', { exact: true })).toBeVisible();
    unlinkSync(path);
    await page.reload();
    await page.setViewportSize({ width: 390, height: 844 });
    await expect(page.getByTestId('acp-activity-head')).toBeDisabled();
    const preview = page.getByTestId('tool-image-preview');
    await expect(preview.locator('img')).toHaveJSProperty('naturalWidth', 1440);
    await preview.click();
    const image = page.getByTestId('tool-detail-image');
    await expect(image).toBeVisible();
    expect(await image.evaluate((img) => img.getBoundingClientRect().width)).toBeLessThanOrEqual(
      390,
    );
    await page.keyboard.press('Shift+Tab');
    const size = page.getByTestId('tool-detail-size');
    await expect(size).toBeFocused();
    await page.keyboard.press('Enter');
    expect(await image.evaluate((img) => img.getBoundingClientRect().width)).toBe(1440);
    await page.keyboard.press('Tab');
    await expect(page.getByTestId('tool-detail-close')).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(size).toBeFocused();
    await page.keyboard.press('Enter');
    expect(await image.evaluate((img) => img.getBoundingClientRect().width)).toBeLessThanOrEqual(
      390,
    );
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
      true,
    );
    await page.getByTestId('tool-detail-close').click();
    await expect(preview).toBeFocused();
  });

  test('adapter user echoes do not duplicate the visible history', async ({ page, weaver }) => {
    // `echo:` re-streams a user turn from the adapter (what claude does after
    // /compact). It must not paint a second "You" message.
    await openAcp(page, weaver, { goal: 'echo:hello from the echo|say:done' });
    const conv = page.getByTestId('acp-conversation');

    await expect(conv.getByText('done', { exact: true })).toBeVisible();
    // The echoed text never renders as its own user message; the one user turn
    // is the dispatched prompt (whose text is the whole script).
    await expect(conv.getByText('hello from the echo', { exact: true })).toHaveCount(0);
    await expect(conv.locator('.acp-label', { hasText: 'You' })).toHaveCount(1);
  });

  test('hands an idle conversation to another ACP provider in place', async ({ page, weaver }) => {
    const s = await openAcp(page, weaver, {
      goal: 'say:before handoff',
      name: 'acp-handoff',
    });
    const conv = page.getByTestId('acp-conversation');
    await expect(conv.getByText('before handoff', { exact: true })).toBeVisible();
    await expect(page.getByTestId('acp-turn-rule')).toBeVisible();
    await defineFakeAcpAgent(weaver, 'acp-fake-next', 'ACP fake next', {
      summaryDelay: 750,
    });
    await fetch(`${weaver.baseUrl}/api/profiles/create`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        name: 'handoff-target',
        description: 'Target provider for handoff',
        agent_kind: 'acp-fake-next',
        model: '',
        effort: '',
        protocol: 'acp',
        mode: 'default',
        class: 'interactive',
        strict: false,
        env_clear: false,
        ambient_allowlist: [],
        max_concurrent: 1,
        prelude: 'weaver',
        restricted: false,
        runtime_permissions: [],
        mcp_access: { mode: 'none', groups: [] },
      }),
    });

    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByTestId('action-handoff').click();
    const form = page.getByTestId('handoff-form');
    await form.getByTestId('profile-option-handoff-target').click();
    await expect(form.getByTestId('resolved-launch-summary')).toContainText('acp-fake-next');
    await form.getByRole('button', { name: 'Hand off now' }).click();

    const progress = page.getByTestId('handoff-progress');
    await expect(progress).toContainText('Handoff in progress');
    await expect(progress).toContainText('Session paused');
    await expect(page.getByTestId('acp-composer')).toHaveCount(0);

    // The stable session URL and earlier prose survive; only the provider and
    // its private ACP connection change. The journal makes that boundary clear.
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}$`));
    await expect.poll(async () => (await weaver.getSession(s.id)).agent_kind).toBe('acp-fake-next');
    await expect(page.getByText('Handed off to acp-fake-next.')).toBeVisible();
    await expect(progress).toHaveCount(0);
    await expect(conv.getByText('before handoff', { exact: true })).toBeVisible();
    await expect
      .poll(async () => {
        const res = await fetch(`${weaver.baseUrl}/api/sessions/chat`, {
          method: 'POST',
          headers: HEADERS,
          body: JSON.stringify({ session: s.id }),
        });
        const chat = (await res.json()) as {
          blocks: Array<{
            kind: string;
            payload: { from?: string; to?: string };
          }>;
        };
        return chat.blocks.find((block) => block.kind === 'handoff')?.payload;
      })
      .toMatchObject({
        from: 'acp-fake',
        to: 'acp-fake-next',
        model: '',
        effort: '',
      });
    await expect(page.getByTestId('acp-handoff')).toContainText('acp-fake → acp-fake-next');

    const input = page.getByTestId('acp-composer-input');
    await input.fill('say:after');
    await page.getByTestId('acp-composer-send').click();
    await expect
      .poll(
        async () => {
          const res = await fetch(`${weaver.baseUrl}/api/sessions/chat`, {
            method: 'POST',
            headers: HEADERS,
            body: JSON.stringify({ session: s.id }),
          });
          const chat = (await res.json()) as {
            blocks: Array<{ kind: string; payload: { text?: string } }>;
          };
          return chat.blocks.some((b) => b.kind === 'agent_message' && b.payload.text === 'after');
        },
        { timeout: 15_000 },
      )
      .toBe(true);
    await expect(conv.getByText('after', { exact: true })).toBeVisible({
      timeout: 15_000,
    });
  });

  test('a run of tool calls folds to one collapsed activity line', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'tool:read:Read config|tool:search:Grep routes|say:surveyed',
    });
    const conv = page.getByTestId('acp-conversation');
    await expect(conv.getByText('surveyed', { exact: true })).toBeVisible();

    // One group line for the whole run, closed by default.
    const head = page.getByTestId('acp-activity-head');
    await expect(head).toHaveCount(1);
    await expect(head).toContainText('2 actions');
    await expect(page.getByTestId('acp-activity-item')).toHaveCount(0);

    await head.click();
    await expect(page.getByTestId('acp-activity-item')).toHaveCount(2);
    await expect(page.locator('.acp-activity-breakdown')).toContainText('1 file read');
    await expect(page.locator('.acp-activity-breakdown')).toContainText('1 search');
  });

  test('gloms interleaved thinking and commands into one activity run', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, {
      goal: 'think:inspect provider versions|tool:read:Read lockfile|think:compare constraints|tool:search:Find callers|say:done',
    });
    const conv = page.getByTestId('acp-conversation');
    await expect(conv.getByText('done', { exact: true })).toBeVisible();

    // Thought/tool alternation is one investigation, not four collapsed rows.
    const head = page.getByTestId('acp-activity-head');
    await expect(head).toHaveCount(1);
    await expect(head).toContainText('2 actions');
    await expect(page.getByTestId('acp-activity-thought')).toHaveCount(0);

    await head.click();
    await expect(page.getByTestId('acp-activity-thought')).toHaveCount(2);
    await expect(page.getByTestId('acp-activity-item')).toHaveCount(2);
  });

  test('keeps long commands out of headings while retaining their full detail without output', async ({
    page,
    weaver,
  }) => {
    const command =
      'kubectl --kubeconfig /home/operator/.kube/cluster --context production -n services exec deployment/api -- printenv';
    await openAcp(page, weaver, { goal: `toolwait:10:${command}|say:done` });
    await expect(page.getByText('done', { exact: true })).toBeVisible();

    const item = page.getByTestId('acp-activity-head');
    const title = item.getByTestId('acp-activity-title');
    await expect(title).toHaveClass(/truncate/);
    await expect(item).toHaveAttribute('aria-expanded', 'false');
    await expect(title).not.toHaveText(command);
    await expect(page.getByTestId('tool-text-preview')).toHaveCount(0);

    await item.click();
    await expect(item).toHaveAttribute('aria-expanded', 'true');
    await expect(page.getByTestId('tool-text-preview')).toHaveText(command);
    await page.getByRole('button', { name: 'Expand command' }).click();
    await expect(page.getByTestId('tool-detail-dialog')).toContainText(command);
    await page.keyboard.press('Escape');
    await expect(page.getByRole('button', { name: 'Expand command' })).toBeFocused();
  });

  test('a failed call keeps its group and output folded during live activity', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'tool:read:Read config|toolfail:cargo test|toolwait:30000:cargo build --workspace',
    });
    await expect(page.getByTestId('acp-working')).toContainText('Tool running');
    await expect(page.getByTestId('acp-status-detail')).toHaveText('cargo build --workspace');
    const group = page.getByTestId('acp-activity-head');
    await expect(group).toHaveAttribute('aria-expanded', 'false');
    await expect(group.getByTestId('acp-activity-failed')).toContainText('1 failed');
    await expect(page.getByTestId('acp-activity-item')).toHaveCount(0);
    await group.click();
    const failed = page.getByTestId('acp-activity-item').filter({ hasText: 'cargo test' }).getByRole('button');
    await expect(failed).toHaveAttribute('aria-expanded', 'false');
    await expect(page.getByTestId('acp-detail')).toHaveCount(0);
    await failed.click();
    await expect(page.getByTestId('acp-detail')).toContainText('exit 1: boom');
    const cause = page.getByTestId('acp-tool-failure');
    await expect(cause).toBeVisible();
    const causeBox = await cause.boundingBox();
    const commandBox = await page.getByTestId('tool-text-preview').first().boundingBox();
    expect(causeBox!.y).toBeLessThan(commandBox!.y);
  });

  test('the live status keeps the newest call through completion, prose, and reload', async ({
    page,
    weaver,
  }) => {
    const command = 'rg -n tool_call crates/loom/frontend/src/components/AcpConversation.vue';
    await openAcp(page, weaver, {
      goal: `tool-overlap:${command}|say:Found the call site.|permission:verify|toolfail:cargo test --workspace|permission:finish`,
    });
    const status = page.getByTestId('acp-working');
    const detail = page.getByTestId('acp-status-detail');
    const allowOnce = page.getByRole('button', { name: 'Allow once', exact: true });
    await expect(allowOnce).toBeVisible();
    await expect(page.getByText('Found the call site.', { exact: true })).toBeVisible();
    await expect(status).toContainText('Tool completed');
    await expect(detail).toHaveText(command);

    await page.setViewportSize({ width: 390, height: 844 });
    await page.reload();
    await expect(allowOnce).toBeVisible();
    await expect(status).toContainText('Tool completed');
    await expect(detail).toHaveText(command);
    await expect(detail).toBeInViewport();
    expect(await detail.evaluate((el) => el.clientWidth === el.scrollWidth)).toBe(true);
    await status.screenshot({ path: test.info().outputPath('acp-tool-status-mobile.png') });

    await allowOnce.click();
    await expect(status).toContainText('Tool failed');
    await expect(detail).toHaveText('cargo test --workspace');
    await allowOnce.click();
    await expect(status).toBeHidden();
  });

  test('progress keeps its order without repeating the agent heading inside a turn', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, {
      goal: 'say:Inspecting the source.|tool:read:Read source|say:Preparing the change.|tool:edit:Update source|say:Change is ready.',
    });
    const conversation = page.getByTestId('acp-conversation');
    await expect(conversation.getByText('Change is ready.', { exact: true })).toBeVisible();
    await expect(conversation.getByLabel('Agent update')).toHaveCount(3);
    await expect(conversation.locator('.acp-label', { hasText: /^Agent$/ })).toHaveCount(1);
    await page.getByTestId('acp-composer-input').fill('say:Second turn reply.');
    await page.getByTestId('acp-composer-send').click();
    await expect(conversation.getByText('Second turn reply.', { exact: true })).toBeVisible();
    await expect(conversation.locator('.acp-label', { hasText: /^Agent$/ })).toHaveCount(2);
  });

  test('the phone composer stays compact, grows with a draft, and keeps settings keyboard accessible', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, { goal: 'say:ready' });
    await page.setViewportSize({ width: 390, height: 844 });
    const composer = page.getByTestId('acp-composer');
    const input = page.getByTestId('acp-composer-input');
    const compactHeight = await composer.evaluate(
      (element) => element.getBoundingClientRect().height,
    );
    expect(compactHeight).toBeLessThan(180);
    await input.fill(Array.from({ length: 10 }, (_, i) => `Draft line ${i}`).join('\n'));
    await expect
      .poll(() => composer.evaluate((element) => element.getBoundingClientRect().height))
      .toBeGreaterThan(compactHeight + 80);
    const draft = await input.inputValue();
    const settings = page.getByTestId('acp-settings-toggle');
    await settings.focus();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('combobox', { name: 'Permissions' })).toBeFocused();
    await expect(input).toHaveValue(draft);
    const panel = await page.getByTestId('acp-settings-panel').boundingBox();
    expect(panel!.x).toBeGreaterThanOrEqual(0);
    expect(panel!.x + panel!.width).toBeLessThanOrEqual(390);
    expect(panel!.y).toBeGreaterThanOrEqual(0);
    await page.keyboard.press('Escape');
    await expect(settings).toBeFocused();
    await input.fill('');
    await expect
      .poll(() => composer.evaluate((element) => element.getBoundingClientRect().height))
      .toBeLessThan(180);
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(
      true,
    );
  });

  test('context links reveal image evidence and the failed action in the selected turn', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, {
      goal: 'tool:read:Read source|image:View image /tmp/screenshot.png|toolfail:cargo test|say:Checking the failure.',
    });
    await expect(page.getByText('Checking the failure.', { exact: true })).toBeVisible();
    await page.route('**/api/sessions/chat', async (route) => {
      const response = await route.fetch();
      const chat = await response.json();
      for (const block of chat.blocks) {
        // A failed read can have no output; the jump must still reveal the failure.
        if (block.kind === 'tool_call' && block.payload.status === 'failed') {
          block.payload.tool_kind = 'read';
          block.payload.content = [];
        }
      }
      await route.fulfill({ response, json: chat });
    });
    await page.reload();
    await page.setViewportSize({ width: 390, height: 844 });
    const activity = page.getByTestId('acp-activity');
    await expect(activity.getByTestId('acp-activity-item')).toHaveCount(0);
    const context = page.getByTestId('acp-context-toggle');
    await context.click();
    await page.getByTestId('acp-context-failures').getByRole('button').click();
    await expect(page.getByTestId('acp-rail')).toHaveCount(0);
    await expect(activity.getByTestId('acp-activity-item')).toHaveCount(3);
    await expect(page.getByTestId('acp-tool-failure')).toBeVisible();
    await expect(activity.getByTestId('acp-activity-item').last().getByRole('button')).toBeFocused();
    await context.click();
    await page.getByTestId('acp-context-images').getByRole('button').click();
    await expect(page.getByTestId('acp-rail')).toHaveCount(0);
    await expect(page.getByTestId('tool-image-preview')).toBeFocused();
    await context.click();
    await page.getByTestId('acp-turns').getByRole('button').click();
    await expect(page.getByTestId('acp-rail')).toHaveCount(0);
    await expect(page.locator('[data-anchor="acp-turn-0"]')).toBeFocused();
  });

  test('an empty conversation shows a styled empty state, not a blank canvas', async ({
    page,
    weaver,
  }) => {
    const s = await launchAcpSession(weaver, {
      goal: 'say:ready',
      name: 'acp-empty',
    });
    test.skip(s === null, SKIP_MSG);
    // Serve an empty journal so the surface renders its fresh-session state.
    // The live tail needs no stub: it is multiplexed into `events.stream` as the
    // `chat:<id>` topic, which only carries frames a working agent produces, and
    // this one has been idle since launch.
    await page.route('**/api/sessions/chat', (route) => {
      const operands = route.request().postDataJSON() as { session?: string };
      if (operands?.session !== s!.id) return route.fallback();
      return route.fulfill({
        json: { blocks: [], live_turn: null, pending_inputs: [] },
      });
    });
    await page.goto(`${weaver.baseUrl}/s/${s!.id}`);
    const empty = page.getByTestId('acp-empty');
    await expect(empty).toBeVisible();
    await expect(empty).toContainText('No conversation yet');
    await page.screenshot({
      path: test.info().outputPath('acp-empty-state.png'),
    });
  });

  test('reconciles the journal tail and pages older history on demand', async ({
    page,
    weaver,
  }) => {
    const s = await launchAcpSession(weaver, {
      goal: 'say:ready',
      name: 'acp-paged',
    });
    test.skip(s === null, SKIP_MSG);
    let snapshots = 0;
    await page.route('**/api/sessions/chat', (route) => {
      const operands = route.request().postDataJSON() as {
        session?: string;
        before_turn?: number;
      };
      if (operands?.session !== s!.id) return route.fallback();
      snapshots += 1;
      const oldest = operands.before_turn === 4;
      const reasoning = operands.before_turn === 8;
      return route.fulfill({
        json: {
          blocks: [
            {
              turn: oldest ? 0 : reasoning ? 4 : 8,
              seq: 0,
              kind: oldest ? 'user_message' : reasoning ? 'thought' : 'agent_message',
              payload: {
                text: oldest ? 'oldest retained prompt' : reasoning ? 'reasoning only' : 'newest retained answer',
              },
              created_at: '2026-07-23T00:00:00Z',
            },
          ],
          older_cursor: oldest ? null : { turn: reasoning ? 4 : 8, seq: 0 },
          live_turn: null,
          effective_mode: null,
          pending_inputs: [],
          metadata: {
            commands: [],
            config_options: [],
            modes: [],
            steering_supported: false,
          },
        },
      });
    });

    await page.goto(`${weaver.baseUrl}/s/${s!.id}`);
    const conversation = page.getByTestId('acp-conversation');
    await expect(conversation.getByText('newest retained answer', { exact: true })).toBeVisible();
    await expect(conversation.getByText('oldest retained prompt', { exact: true })).toHaveCount(0);
    await expect.poll(() => snapshots).toBe(2);

    await page.getByTestId('acp-load-older').click();
    await expect(conversation.getByText('oldest retained prompt', { exact: true })).toBeVisible();
    await expect(page.getByTestId('acp-load-older')).toHaveCount(0);
    expect(snapshots).toBe(4);
  });

  test('loads the latest prompt behind several pages of tools on reload', async ({ page, weaver }) => {
    const session = await openAcp(page, weaver, { goal: 'say:older reply' });
    await expect(page.getByText('older reply', { exact: true })).toBeVisible();

    await queueAcpPrompt(weaver, session, 'toolburst:405');
    await expect(page.getByTestId('acp-turn-rule')).toHaveCount(2);
    await page.reload();

    const conversation = page.getByTestId('acp-conversation');
    await expect(conversation.getByText('toolburst:405', { exact: true })).toBeInViewport();
    const group = conversation.getByTestId('acp-activity-head');
    await expect(group).toContainText('405 actions');
    await expect(group).toHaveAttribute('aria-expanded', 'false');
    await expect(conversation.getByTestId('acp-activity-item')).toHaveCount(0);
  });

  test('an artifact hard refresh does not load the hidden default conversation', async ({
    page,
    weaver,
  }) => {
    const s = await launchAcpSession(weaver, {
      goal: 'say:ready',
      name: 'acp-artifact',
    });
    test.skip(s === null, SKIP_MSG);
    await weaver.writeArtifact(s!, 'proxy-results', '# Proxy results\n\nReady.\n', {
      title: 'Proxy results',
    });
    let chatRequests = 0;
    page.on('request', (request) => {
      const url = new URL(request.url());
      const topics = decodeURIComponent(url.searchParams.get('topics') ?? '').split(',');
      if (url.pathname === '/api/sessions/chat' || topics.includes(`chat:${s!.id}`)) {
        chatRequests += 1;
      }
    });

    await page.goto(`${weaver.baseUrl}/s/${s!.id}/artifacts/proxy-results`);
    await expect(page.locator('.markdown-body h1')).toContainText('Proxy results');
    await page.reload();
    await expect(page.locator('.markdown-body h1')).toContainText('Proxy results');
    // Let any mistakenly mounted conversation component issue its initial request.
    await page.waitForTimeout(500);
    expect(chatRequests).toBe(0);
  });

  test('a live turn shows a status line naming the activity', async ({ page, weaver }) => {
    await page.clock.install();
    await openAcp(page, weaver, { goal: 'say:ready' });
    const input = page.getByTestId('acp-composer-input');
    await input.fill('wait:3500|say:done');
    await page.getByTestId('acp-composer-send').click();

    // The status line sits at the transcript tail, names the activity, and
    // carries the turn + elapsed meta; it clears when the turn ends.
    const status = page.getByTestId('acp-working');
    await expect(status).toBeVisible({ timeout: 15_000 });
    await expect(status).toContainText('Working…');
    await expect(status).toContainText('turn 2');
    await expect(status).toContainText('elapsed');
    await expect(status).toContainText(/updated (just now|0:0[1-9] ago)/);
    await expect(status).toHaveAttribute(
      'title',
      /silence alone does not prove the agent is stuck/,
    );
    // The status reports how long it has been since the last update.
    await page.clock.fastForward(16_000);
    await expect(status).toContainText('no updates for 0:16');
    await page.screenshot({
      path: test.info().outputPath('acp-live-status.png'),
    });
    await expect(status).toBeHidden({ timeout: 15_000 });
  });

  test('Enter returns focus to the composer after sending', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-composer-focus',
    });
    const input = page.getByTestId('acp-composer-input');

    await input.fill('say:first message');
    await input.press('Enter');

    await expect(input).toHaveValue('');
    await expect(input).toBeFocused();
    await input.pressSequentially('say:second message');
    await expect(input).toHaveValue('say:second message');
  });

  test('streaming deltas appear, then consolidate into one message', async ({ page, weaver }) => {
    // A `wait` keeps the turn live long enough for the streamed text to arrive
    // as a delta before its block journals.
    await openAcp(page, weaver, {
      goal: 'wait:600|say:streamed reply lands here',
    });
    const conv = page.getByTestId('acp-conversation');

    // The text shows (whether caught mid-stream as a shadow or after the block
    // journals) and, once the turn ends, reads as a single settled message.
    await expect(conv.getByText('streamed reply lands here', { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByTestId('acp-working')).toBeHidden({
      timeout: 15_000,
    });
    await expect(conv.getByText('streamed reply lands here', { exact: true })).toHaveCount(1);
  });

  test('usage updates do not fragment one agent reply into many speaker rows', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, {
      goal: 'say:The PR override is modeled independently from |usage:41000:200000|say:the cached snapshot.',
      name: 'acp-coalesce',
    });
    const conv = page.getByTestId('acp-conversation');
    await expect(page.getByTestId('acp-working')).toBeHidden({
      timeout: 15_000,
    });
    await expect(
      conv.getByText('The PR override is modeled independently from the cached snapshot.', {
        exact: true,
      }),
    ).toBeVisible();
    await expect(conv.locator('.acp-label', { hasText: 'Agent' })).toHaveCount(1);
    await expect(page.getByTestId('agent-usage')).toContainText('41k / 200k context · 21%');
  });

  test('stop settles working and leaves unseen feedback idle until sent', async ({
    page,
    weaver,
  }) => {
    const session = await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-stop-settles',
    });
    const input = page.getByTestId('acp-composer-input');
    await input.fill('wait:5000|say:unreached');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible({
      timeout: 15_000,
    });
    await queueAcpPrompt(weaver, session, 'say:after stop');
    await expect(page.getByTestId('acp-queued')).toHaveText('queued · agent hasn’t seen this yet');

    await page.getByTestId('acp-composer-stop').click();
    await expect(page.getByTestId('acp-working')).toBeHidden({
      timeout: 2_000,
    });
    await expect(page.getByTestId('acp-turn-rule').last()).toContainText('cancelled');
    await expect(page.getByTestId('acp-queued')).toBeVisible();

    const sendNow = page.getByTestId('acp-force-queued');
    await expect(sendNow).toBeEnabled();
    await expect(sendNow).toHaveText('Send now');
    await sendNow.click();
    await expect(page.getByTestId('acp-queued')).toBeHidden();
    await expect(page.getByText('after stop', { exact: true })).toBeVisible({
      timeout: 15_000,
    });
  });

  test('makes a failed turn obvious and recovers a poisoned session in place', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-recover-error',
    });
    const input = page.getByTestId('acp-composer-input');

    let expectedErrors = 0;
    for (const prompt of ['poison', 'say:never reached']) {
      await input.fill(prompt);
      await page.getByTestId('acp-composer-send').click();
      expectedErrors += 1;
      await expect(page.getByTestId('acp-turn-rule').filter({ hasText: 'error' })).toHaveCount(
        expectedErrors,
        { timeout: 15_000 },
      );
    }

    const errorRule = page.getByTestId('acp-turn-rule').last();
    await expect(errorRule).toHaveClass(/loud/);
    await expect(errorRule).toHaveClass(/error/);
    await expect(errorRule).toHaveAttribute('role', 'alert');
    await expect(page.getByTestId('acp-recover')).toHaveCount(1);
    await expect(page.getByTestId('acp-recover')).toHaveText('Recover');

    await page.getByTestId('acp-recover').click();
    await expect(page.getByTestId('acp-recover')).toHaveText('Recovered', {
      timeout: 15_000,
    });

    await input.fill('say:healthy again');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByText('healthy again', { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByTestId('acp-turn-rule').last()).toContainText('end_turn');
    await expect(page.getByTestId('acp-recover')).toBeHidden();
  });

  for (const stopAndSend of [false, true]) {
    for (const accepted of [true, false]) {
      test(`a slow ${stopAndSend ? 'Stop & send' : 'send'} shows progress and ${accepted ? 'delivers' : 'restores'} the draft`, async ({
        page,
        weaver,
      }) => {
        await openAcp(page, weaver, {
          goal: stopAndSend ? 'wait:30000|say:original' : 'say:ready',
          name: 'acp-slow-send',
        });
        if (stopAndSend) await expect(page.getByTestId('acp-working')).toBeVisible();
        else await expect(page.getByTestId('acp-working')).toBeHidden();
        let release!: () => void;
        const gate = new Promise<void>((resolve) => (release = resolve));
        let acknowledge!: () => void;
        const acknowledgement = new Promise<void>((resolve) => (acknowledge = resolve));
        await page.route('**/api/sessions/prompt/create', async (route) => {
          await gate;
          if (accepted) {
            const response = await route.fetch();
            await acknowledgement;
            await route.fulfill({ response });
          } else await route.fulfill({ status: 409, json: { error: 'Send refused' } });
        });
        const input = page.getByTestId('acp-composer-input');
        await input.fill('say:received');
        if (stopAndSend) await page.getByTestId('acp-composer-stop-send').click();
        else await input.press('Enter');
        try {
          if (stopAndSend) {
            await expect(page.getByTestId('acp-composer-stop-send')).toHaveText('Sending…');
          } else {
            await expect(page.getByTestId('acp-optimistic')).toContainText(
              'sending · waiting for Loom',
            );
          }
          await expect(input).toHaveValue('');
          await expect(page.getByTestId('acp-composer-send')).toBeDisabled();
          release();
          if (accepted) {
            await expect(page.getByText('received', { exact: true })).toBeVisible();
            await expect(input).toHaveValue('');
            await expect(page.getByTestId('acp-composer-send')).toBeDisabled();
          } else {
            await expect(input).toHaveValue('say:received');
            await expect(page.getByText('Send refused', { exact: true })).toBeVisible();
            await expect(page.getByTestId('acp-composer-send')).toBeEnabled();
          }
        } finally {
          release();
          acknowledge();
        }
        await expect(page.getByTestId('acp-optimistic')).toBeHidden();
        await expect(input).toBeEnabled();
      });
    }
  }

  test('Send steers feedback into the live turn without cancelling its tool', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-steering-ui',
      steering: true,
    });
    const input = page.getByTestId('acp-composer-input');
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await input.fill('toolwait:4000:child work|say:original done');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible();
    await input.fill('say:steered feedback');
    await input.press('Enter');
    await expect(page.getByTestId('acp-steered')).toHaveCount(1);
    await expect(page.getByTestId('acp-pending')).toBeHidden();
    await expect(page.getByTestId('acp-conversation')).toContainText('steered feedback', { timeout: 15_000 });
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await expect(page.getByTestId('acp-turn-rule').filter({ hasText: 'cancelled' })).toHaveCount(0);
    await expect(page.getByTestId('acp-conversation')).toContainText('original done');
  });

  test('Send uses the native queue while preserving the running tool', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-native-queue-ui',
      steering: true,
      nativeQueue: true,
    });
    const input = page.getByTestId('acp-composer-input');
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await input.fill('toolwait:4000:child work|say:original done');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible();
    await input.fill('say:queued feedback');
    await input.press('Enter');
    await expect(page.getByTestId('acp-queued')).toHaveText('queueing in agent');
    await expect(page.getByTestId('acp-update-timeout')).toHaveText('Waiting for current turn to finish');
    await expect(page.getByTestId('acp-edit-queued')).toBeHidden();
    await expect(page.getByTestId('acp-force-queued')).toBeHidden();
    await expect(page.getByTestId('acp-pending')).toBeHidden({ timeout: 15_000 });
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await expect(page.getByTestId('acp-conversation').getByText('queued feedback', { exact: true })).toBeVisible();
    await expect(page.getByTestId('acp-conversation')).toContainText('original done');
    await expect(page.getByTestId('acp-turn-rule').filter({ hasText: 'cancelled' })).toHaveCount(0);
  });

  test('composer explicitly stops and sends', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-queue-ui',
    });
    const input = page.getByTestId('acp-composer-input');
    await input.fill('wait:5000|say:first turn done');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible({
      timeout: 15_000,
    });

    await input.fill('say:queued feedback');
    await page.getByTestId('acp-composer-stop-send').click();
    await expect(page.getByTestId('acp-queued')).toBeHidden();
    await expect(page.getByTestId('acp-turn-rule').filter({ hasText: 'cancelled' })).toHaveCount(1);
    await expect(
      page.getByTestId('acp-conversation').getByText('queued feedback', { exact: true }),
    ).toBeVisible({ timeout: 20_000 });
  });

  test('Send queues feedback when steering is unavailable without stopping the live turn', async ({
    page,
    weaver,
  }) => {
    const conv = page.getByTestId('acp-conversation');
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-queue-button',
    });
    const input = page.getByTestId('acp-composer-input');
    // Keep the turn live comfortably beyond scheduling jitter so the send click
    // lands mid-turn, then let it end normally so the queue drains by itself.
    await input.fill('wait:15000|say:first turn done');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible({
      timeout: 15_000,
    });

    await input.fill('say:queued feedback');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-queued')).toHaveText('queued · agent hasn’t seen this yet');
    await expect(page.getByTestId('acp-pending')).toContainText('say:queued feedback');
    await expect(input).toHaveValue('');
    // Normal sends leave the running turn alone.
    await expect(page.getByTestId('acp-working')).toBeVisible();
    await expect(page.getByTestId('acp-turn-rule').filter({ hasText: 'cancelled' })).toHaveCount(0);

    // The normal turn end drains the durable queue into the next turn.
    await expect(conv.getByText('first turn done', { exact: true })).toBeVisible({
      timeout: 30_000,
    });
    await expect(page.getByTestId('acp-queued')).toBeHidden({
      timeout: 15_000,
    });
    await expect(conv.getByText('queued feedback', { exact: true })).toBeVisible({
      timeout: 15_000,
    });
  });

  test('ArrowUp and Edit pull unseen queued feedback back into the composer', async ({
    page,
    weaver,
  }) => {
    const session = await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-edit-queue-ui',
    });
    const input = page.getByTestId('acp-composer-input');
    // Keep the turn live comfortably beyond parallel-run scheduling jitter so
    // this test exercises retraction, not the next-turn queue drain.
    await input.fill('wait:30000|say:first turn done');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible({
      timeout: 15_000,
    });
    await queueAcpPrompt(weaver, session, 'say:before edit');
    const editQueued = page.getByTestId('acp-edit-queued');
    await expect(editQueued).toBeEnabled();

    // Empty-composer ArrowUp follows chat-history convention, but only recalls
    // the message the agent has not seen yet.
    await input.focus();
    await input.press('ArrowUp');
    await expect(page.getByTestId('acp-pending')).toBeHidden();
    await expect(input).toHaveValue('say:before edit');

    await input.fill('');
    await queueAcpPrompt(weaver, session, 'say:after edit');
    const pending = page.getByTestId('acp-pending');
    await expect(pending).toContainText('say:after edit');
    await expect(pending).not.toContainText('say:before edit');

    // The visible action provides the same flow without a keyboard shortcut.
    await expect(editQueued).toBeEnabled();
    await editQueued.click();
    await expect(pending).toBeHidden();
    await expect(input).toHaveValue('say:after edit');
    await expect(input).toBeFocused();
  });

  test('@file completion resolves server files and sends an ACP resource', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-file-resource',
    });
    const input = page.getByTestId('acp-composer-input');
    await expect(input).toHaveAttribute('autocomplete', 'off');
    await input.fill('resources|@READ');
    const menu = page.getByTestId('acp-file-menu');
    await expect(menu.getByText('@README.md', { exact: true })).toBeVisible();
    await menu.getByText('@README.md', { exact: true }).click();
    await expect(input).toHaveValue('resources|');
    await expect(page.getByTestId('acp-attachment-pill')).toContainText('README.md');

    await page.getByTestId('acp-composer-send').click();
    await expect(
      page
        .getByTestId('acp-conversation')
        .getByRole('region', { name: 'Agent update' })
        .getByText('README.md', { exact: true }),
    ).toBeVisible({
      timeout: 15_000,
    });
  });

  test('composer uploads become removable ACP resource pills', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-upload-resource',
    });
    await page.getByTestId('acp-composer-file-input').setInputFiles({
      name: 'long-notes.txt',
      mimeType: 'text/plain',
      buffer: Buffer.from('notes for the agent'),
    });

    const pill = page.getByTestId('acp-attachment-pill');
    await expect(pill).toContainText('scratch/long-notes.txt');
    await expect(page.getByTestId('scratch-panel')).toContainText('long-notes.txt');
    await page.getByTestId('acp-composer-input').fill('resources');
    await page.getByTestId('acp-composer-send').click();
    await expect(
      page
        .getByTestId('acp-conversation')
        .getByRole('region', { name: 'Agent update' })
        .getByText('scratch/long-notes.txt', { exact: true }),
    ).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('acp-attachment-pill')).toHaveCount(0);
  });

  test('composer accepts dropped files as ACP resource pills', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'acp-drop-resource',
    });
    const composer = page.getByTestId('acp-composer');
    // DataTransfer.types may omit Files; the drop payload is the authority.
    const dataTransfer = await page.evaluateHandle(() => {
      const dt = new DataTransfer();
      dt.items.add(
        new File(['dropped notes'], 'dropped-notes.txt', {
          type: 'text/plain',
        }),
      );
      Object.defineProperty(dt, 'types', { value: [] });
      return dt;
    });

    await composer.dispatchEvent('dragenter', { dataTransfer });
    await expect(composer).toHaveClass(/acp-composer--drop/);
    await composer.dispatchEvent('drop', { dataTransfer });

    await expect(composer).not.toHaveClass(/acp-composer--drop/);
    await expect(page.getByTestId('acp-attachment-pill')).toContainText(
      'scratch/dropped-notes.txt',
    );
    await expect(page.getByTestId('scratch-panel')).toContainText('dropped-notes.txt');
  });

  test('discovers slash commands from the agent and forwards them', async ({ page, weaver }) => {
    await openAcp(page, weaver, { goal: 'say:ready' });
    const input = page.getByTestId('acp-composer-input');

    await input.fill('/');
    const menu = page.getByTestId('acp-command-menu');
    await expect(menu.getByText('/resume', { exact: true })).toBeVisible();
    await expect(menu.getByText('/review', { exact: true })).toBeVisible();
    await menu.getByText('/review', { exact: true }).click();
    await expect(input).toHaveValue('/review ');
    await expect(input).toHaveAttribute('placeholder', 'instructions');

    // `/resume` belongs to the adapter, not loom: it is sent as a real prompt
    // and appears once in the durable transcript.
    await input.fill('/resume');
    await page.getByTestId('acp-composer-send').click();
    await expect(
      page.getByTestId('acp-conversation').getByText('/resume', { exact: true }),
    ).toBeVisible();
  });

  test('settings change the live agent config without clearing the draft', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, { goal: 'say:ready' });
    const input = page.getByTestId('acp-composer-input');
    await input.fill('keep this unsent draft');
    const settings = page.getByTestId('acp-settings-toggle');
    await settings.click();

    const permissions = page.getByTestId('acp-config-mode');
    await expect(permissions).toContainText('Permissions');
    await expect(permissions.getByRole('combobox')).toHaveValue('default');
    await permissions.getByRole('combobox').selectOption('acceptEdits');

    const model = page.getByTestId('acp-config-model');
    await expect(model.getByRole('combobox')).toHaveValue('fake-fast');
    await model.getByRole('combobox').selectOption('fake-deep');
    await expect(settings).toContainText('Fake deep');

    const effort = page.getByTestId('acp-config-thought_level');
    await expect(effort.getByRole('combobox')).toHaveValue('medium');
    await effort.getByRole('combobox').selectOption('high');

    const fast = page.getByTestId('acp-config-fast-mode');
    await expect(fast).toContainText('Off');
    await fast.getByRole('checkbox').check();
    await expect(fast).toContainText('On');
    await page.keyboard.press('Escape');
    await expect(settings).toBeFocused();
    await expect(input).toHaveValue('keep this unsent draft');
    await page.reload();
    await settings.click();
    await expect(permissions.getByRole('combobox')).toHaveValue('acceptEdits');
    await expect(model.getByRole('combobox')).toHaveValue('fake-deep');
    await expect(effort.getByRole('combobox')).toHaveValue('high');
    await expect(fast.getByRole('checkbox')).toBeChecked();
    await expect(input).toHaveValue('keep this unsent draft');

    await page.route('**/api/sessions/config/set', (route) =>
      route.fulfill({
        status: 400,
        json: { error: 'The agent rejected this model.' },
      }),
    );
    await model.getByRole('combobox').focus();
    await model.getByRole('combobox').selectOption('fake-fast');
    await expect(page.getByText('The agent rejected this model.', { exact: true })).toBeVisible();
    await expect(model.getByRole('combobox')).toHaveValue('fake-deep');
    await expect(model.getByRole('combobox')).toBeFocused();
    await expect(input).toHaveValue('keep this unsent draft');
  });

  test('a live permission change is labelled for the next turn', async ({ page, weaver }) => {
    await openAcp(page, weaver, { goal: 'say:ready', mode: 'default' });
    const input = page.getByTestId('acp-composer-input');
    await input.fill('wait:5000|say:done');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible({
      timeout: 15_000,
    });

    const permissions = page.getByTestId('acp-config-mode');
    await page.getByTestId('acp-settings-toggle').click();
    await permissions.getByRole('combobox').selectOption('bypassPermissions');
    await expect(permissions.getByRole('combobox')).toHaveValue('bypassPermissions');
    await expect(permissions).toContainText('next turn');
    await page.keyboard.press('Escape');

    await page.getByTestId('acp-composer-stop').click();
    await expect(page.getByTestId('acp-working')).toBeHidden({
      timeout: 5_000,
    });
    await expect(page.getByTestId('acp-settings-toggle')).not.toContainText('next turn');
  });

  test('a permission card answers and collapses to a receipt', async ({ page, weaver }) => {
    // A supervised mode surfaces the request as an interactive card (bypass mode
    // would auto-answer it).
    await openAcp(page, weaver, {
      goal: 'permission:deploy/loom-entrypoint.sh',
      mode: 'default',
    });
    const card = page.getByTestId('acp-permission');
    await expect(card).toBeVisible({ timeout: 15_000 });
    await expect(card).toContainText('deploy/loom-entrypoint.sh');

    // Answering posts the option and collapses the card to a one-line receipt.
    await card.getByTestId('acp-permission-option').filter({ hasText: 'Allow once' }).click();
    await expect(page.getByTestId('acp-permission-receipt')).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByTestId('acp-permission-option')).toHaveCount(0);
  });

  test('the context rail is hidden by default and restores focus when dismissed', async ({
    page,
    weaver,
  }) => {
    await openAcp(page, weaver, { goal: 'plan|say:planned' });
    const toggle = page.getByTestId('acp-context-toggle');
    const rail = page.getByTestId('acp-rail');
    const plan = page.getByTestId('acp-plan');

    await expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await expect(rail).toHaveCount(0);
    await expect(plan).toHaveCount(0);

    await toggle.focus();
    await toggle.press('Enter');
    await expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await expect(rail).toBeVisible();
    await expect(plan).toBeVisible();
    // The fake plan has a completed + an in_progress entry.
    await expect(plan.getByText('first step')).toBeVisible();
    await expect(plan.getByText('second step')).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() => {
          const active = document.activeElement;
          return !!active && document.querySelector('[data-testid="acp-rail"]')?.contains(active);
        }),
      )
      .toBe(true);

    await page.keyboard.press('Escape');
    await expect(rail).toHaveCount(0);
    await expect(toggle).toBeFocused();
  });

  test('expanded ACP details stay reachable in a short viewport', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: `plan|say:${Array.from(
        { length: 36 },
        (_, index) => `Operator context line ${index + 1}`,
      ).join(' ')}`,
      name: 'short-acp-details',
    });
    await page.setViewportSize({ width: 900, height: 320 });

    await page.getByRole('button', { name: /Details/ }).click();
    const scroller = page.getByTestId('details-scroll');
    const goalSummary = page.getByText('Goal / prompt', { exact: true });
    await goalSummary.scrollIntoViewIfNeeded();
    await goalSummary.click();
    await page.getByTestId('session-goal-context').scrollIntoViewIfNeeded();
    await expect(page.getByTestId('session-goal-context')).toBeVisible();

    const handoff = page.getByTestId('action-handoff');
    await handoff.scrollIntoViewIfNeeded();
    await handoff.click();
    const form = page.getByTestId('handoff-form');
    await form.scrollIntoViewIfNeeded();
    await expect(form).toBeVisible();
    await page.getByRole('button', { name: /Details/ }).click();
    await expect(form).toHaveCount(0);
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByTestId('action-handoff').click();
    await expect(page.getByTestId('handoff-form')).toBeVisible();

    const bounds = await scroller.evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      overflowY: getComputedStyle(element).overflowY,
    }));
    expect(bounds.scrollHeight).toBeGreaterThan(bounds.clientHeight);
    expect(bounds.overflowY).toMatch(/auto|scroll/);

    for (const id of ['action-archive', 'action-remove']) {
      const action = page.getByTestId(id);
      await action.scrollIntoViewIfNeeded();
      const box = await action.boundingBox();
      expect(box).not.toBeNull();
      expect(box!.y).toBeGreaterThanOrEqual(0);
      expect(box!.y + box!.height).toBeLessThanOrEqual(320);
    }
  });

  test('renders in both themes', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'think:reasoning|tool:read:Read config|tool:search:Grep routes|tool:edit:Edit web.rs|say:All wired up.|toolfail:cargo test|plan|usage:41000:200000',
    });
    // Capture an open diff alongside a collapsed failed call.
    await expect(page.getByTestId('acp-activity-failed')).toBeVisible();
    await expect(page.getByTestId('acp-detail')).toHaveCount(0);
    await page.getByTestId('acp-activity-head').first().click();
    await page
      .getByTestId('acp-activity-item')
      .filter({ hasText: 'Edit web.rs' })
      .getByRole('button')
      .click();
    await expect(page.getByTestId('acp-diff')).toBeVisible();

    for (const theme of ['light', 'dark'] as const) {
      await page.evaluate((t) => {
        localStorage.setItem('loom-theme', t);
        document.documentElement.classList.toggle('dark', t === 'dark');
      }, theme);
      await page.waitForTimeout(200);
      await page.screenshot({
        path: test.info().outputPath(`acp-conversation-${theme}.png`),
      });
    }
  });

  test('returning from an artifact link keeps the conversation place', async ({
    page,
    weaver,
  }) => {
    const s = await openAcp(page, weaver, { goal: 'say:ready', name: 'acp-back' });
    await weaver.writeArtifact(s, 'plan', '# The plan\n\nShip it.\n', { title: 'The plan' });
    const filler = (start: number, count: number) =>
      Array.from(
        { length: count },
        (_, i) => `Paragraph ${start + i}: enough prose to keep the transcript scrolling.`,
      ).join('\n\n');
    const prompt = [
      'say:ready',
      '',
      filler(1, 12),
      `Details are in the [plan](/s/${s.id}/artifacts/plan).`,
      filler(13, 30),
    ].join('\n\n');
    await page.getByTestId('acp-composer-input').fill(prompt);
    await page.getByTestId('acp-composer-send').click();

    const conv = page.getByTestId('acp-conversation');
    const link = conv.getByRole('link', { name: 'plan' }).first();
    await expect(link).toBeVisible({ timeout: 20_000 });
    // Streaming markdown can replace the link before its durable block lands.
    await expect(page.getByTestId('acp-working')).toBeHidden();
    await link.scrollIntoViewIfNeeded();
    // The link sits below the fold, so the reader is off the foot and the pin
    // has released — the state the return must preserve.
    await expect.poll(() => footDistance(conv)).toBeGreaterThan(120);
    const before = await conv.evaluate((el) => el.scrollTop);

    await link.click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}/artifacts/plan`);
    await expect(page.getByRole('heading', { name: 'The plan' })).toBeVisible();
    await dropHiddenScrollOffset(conv);

    // The session is live: a new turn lands while the reader is on the artifact.
    await queueAcpPrompt(weaver, s, `say:${filler(43, 20).replace(/\n\n/g, ' ')}`);
    await page.waitForTimeout(1_500);

    await page.goBack();
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);
    const returned = page.getByTestId('acp-conversation');
    await expect(returned).toBeVisible();
    await expect
      .poll(() => returned.evaluate((el) => el.scrollTop), { timeout: 5_000 })
      .toBeGreaterThan(before * 0.6);
  });
  // Agents embed absolute paths in their prose (`![mockup](/abs/shot.png)`);
  // the conversation view maps them to the session's image endpoint, which is
  // confined to the session's own worktree. A source that is not there yet
  // (an agent announcing a shot before writing it) degrades to a card naming
  // the path — with a retry that picks the bytes up once they land.
  test('agent prose renders worktree images inline and retries late writes', async ({
    page,
    weaver,
  }) => {
    const s = await openAcp(page, weaver, {
      goal: 'say:ready',
      name: 'inline-worktree-images',
    });
    const shot = join(s.work_dir, '.agent-scratch', 'option-a.png');
    const late = join(s.work_dir, '.agent-scratch', 'late.png');
    const png = readFileSync(join(__dirname, '../../crates/loom/frontend/src/favicon-32.png'));
    try {
      writeFileSync(shot, png);
      await expect(page.getByText('ready', { exact: true })).toBeVisible();

      // Option A's bytes exist; the late shot does not yet.
      await page.getByTestId('acp-composer-input').fill(
        `say:Two renders: ![option A](${shot}) and ![the late one](${late})`,
      );
      await page.getByTestId('acp-composer-send').click();
      const conv = page.getByTestId('acp-conversation');

      // The absolute worktree path resolves to the session's image endpoint…
      const img = conv.locator('img[alt="option A"]').first();
      await expect(img).toHaveAttribute(
        'src',
        `/api/sessions/image?session=${s.id}&path=${encodeURIComponent(shot)}`,
      );
      // …and the served bytes decode: the image really displays.
      await expect
        .poll(() => img.evaluate((el) => (el as HTMLImageElement).naturalWidth))
        .toBeGreaterThan(0);

      // A path with nothing behind it yet names itself instead of showing
      // the broken-image icon.
      const card = conv.getByTestId('markdown-image-fallback').first();
      await expect(card).toContainText(late);
      await expect(card).toContainText('the late one unavailable');

      // The bytes land; one retry remounts the <img> and loads them.
      writeFileSync(late, png);
      await conv.getByTestId('markdown-image-retry').first().click();
      const recovered = conv.locator('img[alt="the late one"]').first();
      await expect
        .poll(() => recovered.evaluate((el) => (el as HTMLImageElement).naturalWidth))
        .toBeGreaterThan(0);
    } finally {
      for (const file of [shot, late]) {
        if (existsSync(file)) unlinkSync(file);
      }
    }
  });
});
