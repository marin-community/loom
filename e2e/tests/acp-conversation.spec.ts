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
  await fetch(`${weaver.baseUrl}/api/agents/custom`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({
      name: 'acp-fake',
      label: 'ACP fake',
      setup: '',
      launch: `node ${FAKE_AGENT}`,
      resume: '',
      reports_status: false,
      protocol: 'acp',
    }),
  }).catch(() => {
    /* already defined by an earlier test in this worker — fine */
  });

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
  test('renders the journaled transcript — user, agent, tool blocks', async ({ page, weaver }) => {
    await openAcp(page, weaver, {
      goal: 'say:The route resolves against auth.base_url now.|tool:edit:Edit web.rs',
    });
    const conv = page.getByTestId('acp-conversation');

    // The agent's consolidated prose (two streamed chunks → one message).
    await expect(conv.getByText('The route resolves against auth.base_url now.')).toBeVisible();
    // An `edit` is consequential — a standalone card, not a census line.
    await expect(page.getByTestId('acp-card').filter({ hasText: 'Edit web.rs' })).toBeVisible();
    // The card carries the diff the adapter streamed as ±lines.
    await expect(page.getByTestId('acp-diff')).toContainText('new line');
  });

  test('streaming deltas appear, then consolidate into one message', async ({ page, weaver }) => {
    // A `wait` keeps the turn live long enough for the streamed text to arrive
    // as a delta before its block journals.
    await openAcp(page, weaver, { goal: 'wait:600|say:streamed reply lands here' });
    const conv = page.getByTestId('acp-conversation');

    // The text shows (whether caught mid-stream as a shadow or after the block
    // journals) and, once the turn ends, reads as a single settled message.
    await expect(conv.getByText('streamed reply lands here')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('acp-working')).toBeHidden({ timeout: 15_000 });
    await expect(conv.getByText('streamed reply lands here')).toHaveCount(1);
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
      goal: 'think:reasoning|tool:read:Read config|say:All wired up.|tool:edit:Edit web.rs|plan|usage:41000:200000',
    });
    await expect(page.getByTestId('acp-card').first()).toBeVisible();

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
