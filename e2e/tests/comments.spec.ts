import { test, expect } from '../fixtures/weaver';
import type { Page } from '@playwright/test';

// The Wave-style collaborative layer on artifacts: the session goal is now a
// first-class `goal` artifact (versioned, rendered, commentable), and any
// markdown artifact carries an inline comment layer — select-to-comment, a
// highlighted span, and a reply thread that renders in the document flow right
// under the block it annotates (not a margin gutter), with resolve dropping the
// thread out of the rendered view while keeping it in history. The comment
// backend is exercised directly by the Rust suite; this drives the real browser
// UI: text-quote anchoring, the CSS Custom Highlight paint, and the inline cards
// teleported into the rendered body.

const DOC = [
  '# Design notes',
  '',
  'We keep the markdown representation as the default, and layer',
  'collaborative editing on top of it.',
  '',
  '## Open questions',
  '',
  '- Should comments resolve out of the agent context?',
  '- How do anchors survive an edit elsewhere in the document?',
  '',
].join('\n');

/** Select a phrase inside the rendered `.markdown-body` and fire the `mouseup`
 *  the comment controller listens for, so the floating "Comment" button shows. */
async function selectPhrase(page: Page, phrase: string) {
  await page.evaluate((needle) => {
    const body = document.querySelector('.markdown-body') as HTMLElement;
    const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
    let node: Text | null = null;
    let idx = -1;
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const at = (n as Text).data.indexOf(needle);
      if (at !== -1) {
        node = n as Text;
        idx = at;
        break;
      }
    }
    if (!node) throw new Error(`phrase not found in rendered body: ${needle}`);
    const range = document.createRange();
    range.setStart(node, idx);
    range.setEnd(node, idx + needle.length);
    const sel = window.getSelection()!;
    sel.removeAllRanges();
    sel.addRange(range);
    body.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  }, phrase);
}

test.describe('goal as an artifact', () => {
  test('a seeded goal becomes a first-class `goal` artifact you can open', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: '# Ship the search rewrite\n\nMake it **fast** and incremental.',
      name: 'goal-artifact',
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/goal`);

    // The goal shows in the artifact list as a branch-scoped artifact, and its
    // latest revision renders as markdown — the single source of truth the
    // session-create path wrote through `set_goal`.
    const row = page.locator('[data-artifact="goal"]');
    await expect(row).toBeVisible();
    await expect(row).toContainText('branch');
    const body = page.locator('.markdown-body');
    await expect(body.locator('h1')).toContainText('Ship the search rewrite');
    await expect(body.locator('strong')).toContainText('fast');
  });
});

test.describe('artifact inline comments', () => {
  test('select → comment → reply → resolve, with a painted highlight', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'commenting', name: 'comments' });
    await weaver.writeArtifact(session, 'design', DOC, { title: 'Design notes' });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await expect(page.locator('.markdown-body h1')).toContainText('Design notes');

    // Select a phrase and open the composer from the floating button.
    await selectPhrase(page, 'collaborative editing');
    const commentBtn = page.getByTestId('comment-select-button');
    await expect(commentBtn).toBeVisible();
    await commentBtn.click();

    const composer = page.getByTestId('comment-pending');
    await expect(composer).toBeVisible();
    await composer.locator('textarea').fill('Do we keep WYSIWYG too, or source-only?');
    await composer.getByRole('button', { name: 'Comment' }).click();

    // A card appears (active/expanded) carrying the anchor quote and the body.
    const card = page.locator('[data-testid^="comment-card-"]').first();
    await expect(card).toBeVisible();
    await expect(card).toContainText('collaborative editing');
    await expect(card).toContainText('Do we keep WYSIWYG too');

    // The span is painted via the CSS Custom Highlight API.
    const painted = await page.evaluate(
      () => 'highlights' in CSS && (CSS.highlights as Map<string, unknown>).has('weaver-comment'),
    );
    expect(painted).toBe(true);

    // A reply appends to the same thread.
    await card.locator('textarea').fill('Source-only to start.');
    await card.getByRole('button', { name: 'Reply' }).click();
    await expect(card).toContainText('Source-only to start.');

    // Resolve drops the thread out of the rendered view (still in history via
    // the API) — no card, and the highlight is cleared.
    await card.getByRole('button', { name: 'Resolve' }).click();
    await expect(page.locator('[data-testid^="comment-card-"]')).toHaveCount(0);
    const clearedAfterResolve = await page.evaluate(
      () => 'highlights' in CSS && (CSS.highlights as Map<string, unknown>).has('weaver-comment'),
    );
    expect(clearedAfterResolve).toBe(false);
  });

  test('captures the comment UI in both themes', async ({ page, weaver }) => {
    const shotDir = process.env.WEAVER_SHOT_DIR;

    for (const t of ['light', 'dark'] as const) {
      // A fresh session per theme so each shot is a clean single inline thread,
      // not one carrying a leftover collapsed card from the previous iteration.
      const session = await weaver.seedSession({ goal: 'shots', name: `comments-shot-${t}` });
      await weaver.writeArtifact(session, 'design', DOC, { title: 'Design notes' });

      await page.addInitScript((theme) => localStorage.setItem('loom-theme', theme), t);
      await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
      await expect(page.locator('.markdown-body h1')).toContainText('Design notes');

      await selectPhrase(page, 'collaborative editing');
      await page.getByTestId('comment-select-button').click();
      const composer = page.getByTestId('comment-pending');
      await composer.locator('textarea').fill('Anchor survives edits — recovery-based re-locate?');
      await composer.getByRole('button', { name: 'Comment' }).click();
      // Wait for the create to land: the composer closes and the thread's own
      // inline card (expanded, carrying the body) takes its place.
      await expect(composer).toBeHidden();
      const card = page.locator('[data-testid^="comment-card-"]').first();
      await expect(card).toContainText('Anchor survives edits');

      if (shotDir) {
        await page.screenshot({ path: `${shotDir}/comments-${t}.png`, fullPage: false });
      }
    }
  });
});
