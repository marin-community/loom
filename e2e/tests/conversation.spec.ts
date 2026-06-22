import type { Page } from '@playwright/test';
import { test, expect } from '../fixtures/weaver';
import type { WeaverFixture } from '../fixtures/weaver';

// The Conversation tab renders a session's normalized iris log as a *skimmable*
// review surface: prose stays in view, the agent's machinery (tool calls +
// outputs, thinking, context) folds away collapsed, runs of the same tool call
// are run-length collapsed, and a right-hand prompt index jumps between turns.

/** A small iris log: three user turns, a thinking block, a folded context
 *  block, a single Bash call, and a burst of ten identical TaskCreate calls. */
function demoLog() {
  const tasks: unknown[] = [];
  for (let i = 0; i < 10; i++) {
    tasks.push({ kind: 'tool_use', name: 'TaskCreate', input: { title: `subtask ${i + 1}` } });
    tasks.push({ kind: 'tool_result', output: `Created #${100 + i}`, is_error: false });
  }
  return {
    source: 'claude',
    model: 'claude-opus-4-8',
    messages: [
      { role: 'context', blocks: [{ kind: 'text', text: 'Session primer — follow AGENTS.md.' }] },
      { role: 'user', timestamp: '2026-06-21T10:00:00.000Z', blocks: [{ kind: 'text', text: 'Add a dark-mode toggle.' }] },
      {
        role: 'assistant',
        timestamp: '2026-06-21T10:00:01.000Z',
        blocks: [
          { kind: 'thinking', text: 'Let me find the theme store first.' },
          { kind: 'text', text: 'Looking at the settings view.' },
          { kind: 'tool_use', name: 'Bash', input: { command: 'rg -n theme src/' } },
          { kind: 'tool_result', output: 'src/theme.ts:12', is_error: false },
          { kind: 'text', text: 'Filing tasks for the work.' },
          ...tasks,
        ],
      },
      { role: 'user', timestamp: '2026-06-21T10:05:00.000Z', blocks: [{ kind: 'text', text: 'Make it persist.' }] },
      { role: 'assistant', timestamp: '2026-06-21T10:05:01.000Z', blocks: [{ kind: 'text', text: 'It persists via the store.' }] },
      { role: 'user', timestamp: '2026-06-21T10:09:00.000Z', blocks: [{ kind: 'text', text: 'Ship it.' }] },
      { role: 'assistant', timestamp: '2026-06-21T10:09:01.000Z', blocks: [{ kind: 'text', text: 'Opening the PR.' }] },
    ],
  };
}

async function openConversation(page: Page, weaver: WeaverFixture) {
  const s = await weaver.seedSession({ goal: 'demo', name: 'conv' });
  await weaver.seedConversation(s, demoLog());
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(`${weaver.baseUrl}/s/${s.id}`);
  await page.locator('[data-tab="conversation"]').click();
  await expect(page.getByTestId('conversation')).toBeVisible();
  return s;
}

test.describe('conversation view', () => {
  test('folds tool activity and run-length collapses repeated calls', async ({ page, weaver }) => {
    await openConversation(page, weaver);
    const conv = page.getByTestId('conversation');

    // The ten TaskCreate calls collapse to a single fold with a 10× count, not
    // ten separate rows. Bash and TaskCreate are the only two tool folds.
    await expect(page.getByTestId('tool-fold')).toHaveCount(2);
    await expect(page.getByTestId('rle-count')).toHaveText('10×');

    // Folds are collapsed by default — the collapsed header shows a one-line
    // command preview, but the tool's *output* stays hidden until expanded.
    await expect(conv.getByText('rg -n theme src/', { exact: false })).toBeVisible();
    await expect(conv.getByText('src/theme.ts:12')).toHaveCount(0);
    await page.getByTestId('tool-fold').filter({ hasText: 'Bash' }).getByRole('button').first().click();
    await expect(conv.getByText('src/theme.ts:12')).toBeVisible();
  });

  test('category filters strip tool / thinking noise on demand', async ({ page, weaver }) => {
    await openConversation(page, weaver);

    // Hiding tools removes every tool fold, leaving just the prose.
    await page.getByRole('button', { name: 'Toggle tool calls' }).click();
    await expect(page.getByTestId('tool-fold')).toHaveCount(0);
    // The agent's prose still reads.
    await expect(page.getByText('It persists via the store.')).toBeVisible();

    // Toggling it back restores them.
    await page.getByRole('button', { name: 'Toggle tool calls' }).click();
    await expect(page.getByTestId('tool-fold')).toHaveCount(2);
  });

  test('expand-all opens every fold, collapse-all closes them', async ({ page, weaver }) => {
    await openConversation(page, weaver);
    const conv = page.getByTestId('conversation');

    await page.getByRole('button', { name: 'Expand all' }).click();
    await expect(conv.getByText('src/theme.ts:12')).toBeVisible();
    await expect(conv.getByText('Let me find the theme store first.')).toBeVisible();

    await page.getByRole('button', { name: 'Collapse all' }).click();
    await expect(conv.getByText('src/theme.ts:12')).toHaveCount(0);
  });

  test('the prompt index lists every user turn and highlights on jump', async ({ page, weaver }) => {
    await openConversation(page, weaver);

    const items = page.getByTestId('conversation-toc-item');
    await expect(items).toHaveCount(3);
    await expect(items.first()).toContainText('Add a dark-mode toggle.');

    // The first prompt is active on load; clicking the third makes it active.
    await expect(items.first()).toHaveAttribute('data-active', 'true');
    await items.nth(2).click();
    await expect(items.nth(2)).toHaveAttribute('data-active', 'true');
    await expect(items.first()).toHaveAttribute('data-active', 'false');
  });

  // The composer drives the live agent: a live (`running`) session shows a text
  // box at the foot that types a new prompt straight into the agent pane.
  test('the composer sends a new prompt to the agent and clears the input', async ({
    page,
    weaver,
  }) => {
    const s = await openConversation(page, weaver);

    await expect(page.getByTestId('conversation-composer')).toBeVisible();
    const input = page.getByTestId('composer-input');
    await input.fill('please run the tests');
    await page.getByTestId('composer-send').click();

    // The input clears once the send resolves…
    await expect(input).toHaveValue('');
    // …and the backend recorded the `nudge` audit event for the typed text (the
    // send → type-into-the-pane primitive that `POST /sessions/{id}/send` wraps).
    await expect
      .poll(async () => {
        const res = await fetch(`${weaver.baseUrl}/api/sessions/${s.id}/log`);
        const log = (await res.json()) as { kind: string; data: { text?: string } }[];
        return log.some((e) => e.kind === 'nudge' && e.data?.text === 'please run the tests');
      })
      .toBe(true);
  });

  // The tab follows a live session: a new turn landing in the transcript shows
  // up without a manual Refresh, driven off the agent's lifecycle SSE edges.
  test('the log auto-refreshes when the agent reaches a turn boundary', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'demo', name: 'conv-live' });
    await weaver.seedConversation(s, demoLog());
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.locator('[data-tab="conversation"]').click();
    await expect(page.getByTestId('conversation')).toBeVisible();
    await expect(page.getByText('A fresh reply just landed.')).toHaveCount(0);

    // A reply the reviewer hasn't seen is appended to the captured log…
    const base = demoLog();
    const grown = {
      ...base,
      messages: [
        ...base.messages,
        {
          role: 'assistant',
          timestamp: '2026-06-21T10:12:00.000Z',
          blocks: [{ kind: 'text', text: 'A fresh reply just landed.' }],
        },
      ],
    };
    await weaver.seedConversation(s, grown);

    // …and an agent lifecycle edge (an `idle` hook → a `tag` SSE event) makes the
    // tab re-fetch on its own — no Refresh click.
    await weaver.hook(s, 'idle');
    await expect(page.getByText('A fresh reply just landed.')).toBeVisible({ timeout: 15_000 });
  });
});
