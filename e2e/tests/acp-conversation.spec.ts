import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures/weaver';
import type { Session, WeaverFixture } from '../fixtures/weaver';
import { join } from 'path';

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
//   plan                 a plan update
//   usage:USED:SIZE      a usage_update
//   wait:MS              sleep (keeps the turn live — for queue/interrupt tests)
//   permission:NAME      a session/request_permission that blocks the turn
//
// The ACP *lifecycle* backend (weaver #508 — the protocol axis on agents +
// sessions, create/adopt branching, `--mode`) is a parallel phase. Until it
// merges, the server can't launch an acp session over REST, so `launchAcpSession`
// returns null and every test self-skips with a clear message. The specs are
// written to activate unmodified once that phase lands — the only backend-shaped
// assumptions (how a custom agent declares `protocol`/`launch`, and the create
// body's `protocol`/`mode` fields) live in `launchAcpSession`.

const FAKE_AGENT = join(__dirname, '..', '..', 'crates', 'loom', 'tests', 'fixtures', 'fake-acp-agent.mjs');
const HEADERS = { 'content-type': 'application/json' };
const SKIP_MSG = 'ACP lifecycle backend (weaver #508) not merged: the server does not launch acp sessions over REST yet';

async function defineFakeAcpAgent(weaver: WeaverFixture, name: string, label: string): Promise<void> {
  const res = await fetch(`${weaver.baseUrl}/api/agents/custom`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({
      name,
      label,
      setup: '',
      launch: `node ${FAKE_AGENT}`,
      resume: '',
      reports_status: false,
      protocol: 'acp',
    }),
  });
  // Tests sharing a worker can race to define the common source adapter.
  if (!res.ok && res.status !== 409) throw new Error(`defining ${name} failed: ${await res.text()}`);
}

/** Define (idempotently) a custom agent that runs the fake ACP adapter over
 *  stdio, then create a session with it. Returns the session when the backend
 *  brought it up as `protocol='acp'`, else null (the acp axis isn't wired yet). */
async function launchAcpSession(
  weaver: WeaverFixture,
  opts: { goal: string; mode?: string; name?: string },
): Promise<Session | null> {
  // The fake adapter is launched by the custom agent's `launch` command. The
  // `protocol` axis marks it ACP rather than a PTY TUI (added by the lifecycle
  // phase; ignored by the current backend, which is exactly why the probe below
  // detects support).
  await defineFakeAcpAgent(weaver, 'acp-fake', 'ACP fake');

  const res = await fetch(`${weaver.baseUrl}/api/sessions`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({
      goal: opts.goal,
      cwd: weaver.repoPath,
      agent: 'acp-fake',
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
  opts: { goal: string; mode?: string; name?: string },
): Promise<Session> {
  const s = await launchAcpSession(weaver, opts);
  test.skip(s === null, SKIP_MSG);
  await page.setViewportSize({ width: 1360, height: 900 });
  await page.goto(`${weaver.baseUrl}/s/${s!.id}`);
  await expect(page.getByTestId('acp-conversation')).toBeVisible();
  return s!;
}

test.describe('acp conversation', () => {
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
    await expect(page.getByTestId('acp-turn-rule')).toBeVisible({ timeout: 20_000 });

    // The transcript genuinely overflows… (generous timeouts: markdown paints
    // asynchronously and can lag well behind the stream on a loaded machine)
    await expect
      .poll(() => conv.evaluate((el) => el.scrollHeight - el.clientHeight), { timeout: 30_000 })
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
      conv.getByText('The route resolves against auth.base_url now.', { exact: true }),
    ).toBeVisible();
    // The edit folds into an activity line, closed by default — no output visible.
    const head = page.getByTestId('acp-activity-head').filter({ hasText: 'Edit web.rs' });
    await expect(head).toBeVisible();
    await expect(page.getByTestId('acp-diff')).toBeHidden();
    // Opening the group lists the call; opening the call reveals its diff as ±lines.
    await head.click();
    const item = page.getByTestId('acp-activity-item').filter({ hasText: 'Edit web.rs' });
    await expect(item).toBeVisible();
    await item.getByRole('button').click();
    await expect(page.getByTestId('acp-diff')).toContainText('new line');
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
    await defineFakeAcpAgent(weaver, 'acp-fake-next', 'ACP fake next');

    await page.getByRole('button', { name: 'manage' }).click();
    await page.getByTestId('action-handoff').click();
    const form = page.getByTestId('handoff-form');
    await form.getByLabel('Provider').selectOption('acp-fake-next');
    await form.getByRole('button', { name: 'Hand off now' }).click();

    // The stable session URL and earlier prose survive; only the provider and
    // its private ACP connection change. The journal makes that boundary clear.
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}$`));
    await expect.poll(async () => (await weaver.getSession(s.id)).agent_kind).toBe('acp-fake-next');
    await expect(page.getByText('acp-fake-next', { exact: true })).toBeVisible();
    await expect(conv.getByText('before handoff', { exact: true })).toBeVisible();
    await expect(page.getByTestId('acp-handoff')).toContainText('acp-fake → acp-fake-next');

    const input = page.getByTestId('acp-composer-input');
    await input.fill('say:after');
    await page.getByTestId('acp-composer-send').click();
    await expect
      .poll(
        async () => {
          const res = await fetch(`${weaver.baseUrl}/api/sessions/${s.id}/chat`);
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
    await expect(head).toContainText('2 steps');
    await expect(head).toContainText('1 read');
    await expect(head).toContainText('1 search');
    await expect(page.getByTestId('acp-activity-item')).toHaveCount(0);

    await head.click();
    await expect(page.getByTestId('acp-activity-item')).toHaveCount(2);
  });

  test('a failed call opens its group and shows the failure', async ({ page, weaver }) => {
    await openAcp(page, weaver, { goal: 'tool:read:Read config|toolfail:cargo test|say:after' });
    const conv = page.getByTestId('acp-conversation');
    await expect(conv.getByText('after', { exact: true })).toBeVisible();

    // The failure re-opens the fold by default: the badge, the failed line, and
    // its output are all visible without a click.
    await expect(page.getByTestId('acp-activity-failed')).toContainText('1 failed');
    await expect(page.getByTestId('acp-activity-item').filter({ hasText: 'cargo test' })).toContainText(
      'failed',
    );
    await expect(page.getByTestId('acp-detail')).toContainText('exit 1: boom');
  });

  test('an empty conversation shows a styled empty state, not a blank canvas', async ({
    page,
    weaver,
  }) => {
    const s = await launchAcpSession(weaver, { goal: 'say:ready', name: 'acp-empty' });
    test.skip(s === null, SKIP_MSG);
    // Serve an empty journal so the surface renders its fresh-session state.
    await page.route(`**/api/sessions/${s!.id}/chat`, (route) =>
      route.fulfill({ json: { blocks: [], live_turn: null } }),
    );
    await page.route(`**/api/sessions/${s!.id}/chat/stream`, (route) =>
      route.fulfill({ status: 200, headers: { 'content-type': 'text/event-stream' }, body: '' }),
    );
    await page.goto(`${weaver.baseUrl}/s/${s!.id}`);
    const empty = page.getByTestId('acp-empty');
    await expect(empty).toBeVisible();
    await expect(empty).toContainText('No conversation yet');
    await page.screenshot({ path: test.info().outputPath('acp-empty-state.png') });
  });

  test('a live turn shows a status line naming the activity', async ({ page, weaver }) => {
    await openAcp(page, weaver, { goal: 'say:ready' });
    const input = page.getByTestId('acp-composer-input');
    await input.fill('wait:2500|say:done');
    await page.getByTestId('acp-composer-send').click();

    // The status line sits at the transcript tail, names the activity, and
    // carries the turn + elapsed meta; it clears when the turn ends.
    const status = page.getByTestId('acp-working');
    await expect(status).toBeVisible({ timeout: 15_000 });
    await expect(status).toContainText('Working…');
    await expect(status).toContainText('turn 2');
    await page.screenshot({ path: test.info().outputPath('acp-live-status.png') });
    await expect(status).toBeHidden({ timeout: 15_000 });
  });

  test('streaming deltas appear, then consolidate into one message', async ({ page, weaver }) => {
    // A `wait` keeps the turn live long enough for the streamed text to arrive
    // as a delta before its block journals.
    await openAcp(page, weaver, { goal: 'wait:600|say:streamed reply lands here' });
    const conv = page.getByTestId('acp-conversation');

    // The text shows (whether caught mid-stream as a shadow or after the block
    // journals) and, once the turn ends, reads as a single settled message.
    await expect(conv.getByText('streamed reply lands here', { exact: true })).toBeVisible({
      timeout: 15_000,
    });
    await expect(page.getByTestId('acp-working')).toBeHidden({ timeout: 15_000 });
    await expect(conv.getByText('streamed reply lands here', { exact: true })).toHaveCount(1);
  });

  test('the composer sends a prompt and queues one behind a live turn', async ({ page, weaver }) => {
    await openAcp(page, weaver, { goal: 'say:ready' });
    const input = page.getByTestId('acp-composer-input');

    // A first prompt that stays in flight (the `wait`).
    await input.fill('wait:1500|say:first turn done');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-working')).toBeVisible({ timeout: 15_000 });

    // A second prompt sent mid-turn queues visibly rather than starting a turn.
    await input.fill('say:second turn');
    await page.getByTestId('acp-composer-send').click();
    await expect(page.getByTestId('acp-queued')).toBeVisible({ timeout: 15_000 });

    // It dispatches once the first turn ends.
    await expect(page.getByTestId('acp-conversation').getByText('second turn')).toBeVisible({
      timeout: 20_000,
    });
  });

  test('a permission card answers and collapses to a receipt', async ({ page, weaver }) => {
    // A supervised mode surfaces the request as an interactive card (bypass mode
    // would auto-answer it).
    await openAcp(page, weaver, { goal: 'permission:deploy/loom-entrypoint.sh', mode: 'default' });
    const card = page.getByTestId('acp-permission');
    await expect(card).toBeVisible({ timeout: 15_000 });
    await expect(card).toContainText('deploy/loom-entrypoint.sh');

    // Answering posts the option and collapses the card to a one-line receipt.
    await card.getByTestId('acp-permission-option').filter({ hasText: 'Allow once' }).click();
    await expect(page.getByTestId('acp-permission-receipt')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('acp-permission-option')).toHaveCount(0);
  });

  test('the plan rail renders the latest checklist', async ({ page, weaver }) => {
    await openAcp(page, weaver, { goal: 'plan|say:planned' });
    const plan = page.getByTestId('acp-plan');
    await expect(plan).toBeVisible();
    // The fake plan has a completed + an in_progress entry.
    await expect(plan.getByText('first step')).toBeVisible();
    await expect(plan.getByText('second step')).toBeVisible();
  });

  test('renders in both themes', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'think:reasoning|tool:read:Read config|tool:search:Grep routes|tool:edit:Edit web.rs|say:All wired up.|toolfail:cargo test|plan|usage:41000:200000',
    });
    // The grouped run + the auto-opened failure are both on screen; expand the
    // group and one diff so the screenshot shows every fold state.
    await expect(page.getByTestId('acp-detail')).toBeVisible();
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
      await page.screenshot({ path: test.info().outputPath(`acp-conversation-${theme}.png`) });
    }
  });
});
