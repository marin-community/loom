import { test, expect } from '../fixtures/weaver';

// Artifacts are the agent's out-of-repo documents: named, scoped (branch vs
// repo-shared), versioned by immutable snapshot, rendered by loom. This drives a
// real `plan` artifact through the running UI — the list, the markdown viewer
// with the smartdoc projection (an `#N` issue ref becomes a live status chip),
// the version picker, an SSE-driven refresh, a user edit that appends a
// revision, and the docked/popped document layouts.

/** A `plan` artifact referencing issue #id, with a mermaid diagram and an
 *  issue-like token inside a code span that must NOT be projected. */
function planDoc(id: number, marker: string): string {
  return [
    `# Search rewrite ${marker}`,
    '',
    '## Architecture',
    '```mermaid',
    'flowchart TD',
    '  api --> ui',
    '```',
    '',
    '## Tasks',
    `- #${id} Index layer — storage + read path`,
    '- [ ] decide single-node vs distributed',
    '',
    'Write a literal reference in prose with `#999` — it stays plain text.',
    '',
  ].join('\n');
}

function longDoc(): string {
  return [
    '# Long operational notes',
    '',
    ...Array.from(
      { length: 180 },
      (_, i) =>
        `## Checkpoint ${i + 1}\n\nThis is enough prose to make the document overflow its workbench rail.`,
    ),
    '',
  ].join('\n');
}

test.describe('artifacts surface', () => {
  test('renders the viewer with projection, mermaid, and version history', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'rewrite search',
      name: 'artifacts-view',
    });
    const issue = await weaver.seedIssue(session, 'Index layer');

    // Two revisions so the picker has history; the second retitles via the env.
    await weaver.writeArtifact(session, 'plan', planDoc(issue.id, 'v1'), {
      title: 'Search rewrite',
    });
    await weaver.writeArtifact(session, 'plan', planDoc(issue.id, 'v2'), {
      title: 'Search rewrite',
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/plan`);

    // The list shows the branch-scoped `plan` at its latest revision.
    const row = page.locator('[data-artifact="plan"]');
    await expect(row).toBeVisible();
    await expect(row).toContainText('branch'); // scope badge
    await expect(row).toContainText('v2');

    // The viewer renders the markdown (heading carries its slugged anchor).
    const body = page.locator('.markdown-body');
    await expect(body.locator('h1')).toContainText('Search rewrite');

    // smartdoc projection: the `#N` task ref becomes a live chip linking to the
    // issue, its state read from the ledger (open + claimed by this branch).
    const chip = body.locator(`a.smartdoc-chip[data-issue="${issue.id}"]`);
    await expect(chip).toBeVisible();
    await expect(chip).toHaveText(`#${issue.id}`);
    await expect(chip).toHaveAttribute('data-status', 'open');
    await expect(chip).toHaveClass(/smartdoc-chip--claimed/);

    // The `#999` inside the code span is documentation, never a chip.
    await expect(body.locator('a[data-issue="999"]')).toHaveCount(0);
    await expect(body.locator('code', { hasText: '#999' })).toBeVisible();

    // The architecture diagram renders to SVG (lazy bundle + async render).
    const mermaid = body.locator('.mermaid-diagram svg');
    await expect(mermaid).toBeVisible({ timeout: 30_000 });
    await expect(mermaid.locator('g').first()).toBeVisible({ timeout: 30_000 });

    // The version picker defaults to latest and offers each revision.
    const rev = page.getByTestId('artifact-rev');
    await expect(rev.locator('option')).toContainText(['latest (v2)', 'v2', 'v1']);

    // Selecting an older revision re-fetches it read-only; Edit is disabled.
    await rev.selectOption('1');
    await expect(page.getByText('Viewing an older revision')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Edit', exact: true })).toBeDisabled();

    // An out-of-band CLI write (rev 3) is re-broadcast over SSE; the viewer,
    // back on latest, refreshes itself. Return to latest first.
    await rev.selectOption('');
    await weaver.writeArtifact(session, 'plan', planDoc(issue.id, 'v3'), {
      title: 'Search rewrite',
    });
    await expect(rev.locator('option').first()).toHaveText(/latest \(v3\)/, {
      timeout: 15_000,
    });
  });

  test('a user edit in the viewer appends a revision', async ({ page, weaver }) => {
    const session = await weaver.seedSession({
      goal: 'edit me',
      name: 'artifacts-edit',
    });
    await weaver.writeArtifact(session, 'notes', '# Notes\n\nFirst draft.\n', {
      title: 'Notes',
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/notes`);
    await expect(page.locator('.markdown-body h1')).toContainText('Notes');

    // Edit flips the viewer to a raw-source textarea; append a line and save.
    await page.getByRole('button', { name: 'Edit', exact: true }).click();
    const editor = page.getByTestId('artifact-source-editor');
    await expect(editor).toBeVisible();
    await editor.fill((await editor.inputValue()) + '\n\nA user revision.');
    await page.getByRole('button', { name: 'Save' }).click();

    // The save appended rev 2 (author: user); the picker and list reflect it.
    await expect(page.getByTestId('artifact-rev').locator('option').first()).toHaveText(
      /latest \(v2\)/,
    );
    await expect(page.locator('[data-artifact="notes"]')).toContainText('v2');
    // Back in preview, the edited content shows.
    await expect(page.locator('.markdown-body')).toContainText('A user revision.');
  });

  test('deleting an artifact removes it and falls back to the next', async ({ page, weaver }) => {
    const session = await weaver.seedSession({
      goal: '# Session goal\n\nClean up the docs.',
      name: 'artifacts-delete',
    });
    await weaver.writeArtifact(session, 'keep', '# Keep me\n', {
      title: 'Keep',
    });
    await weaver.writeArtifact(session, 'scratch', '# Throwaway\n', {
      title: 'Scratch',
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/scratch`);
    await expect(page.locator('.markdown-body h1')).toContainText('Throwaway');

    // Confirm the destructive action, then delete.
    await page.getByTestId('artifact-delete').click();
    await page.getByTestId('confirm-dialog-confirm').click();

    // The row is gone and the viewer falls back to the first remaining artifact.
    // Every session carries an always-present `goal` artifact (the goal is a
    // first-class artifact), which sorts ahead of user docs — so the fallback
    // lands on it, and its markdown renders in the viewer.
    await expect(page.locator('[data-artifact="scratch"]')).toHaveCount(0);
    await expect(page.locator('[data-artifact="keep"]')).toBeVisible();
    await expect(page.locator('.markdown-body h1')).toContainText('Session goal');
  });

  test('a CLI `artifacts delete` removes it and the list updates over SSE', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'cli rm',
      name: 'artifacts-cli-rm',
    });
    await weaver.writeArtifact(session, 'doomed', '# Doomed\n', {
      title: 'Doomed',
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/doomed`);
    await expect(page.locator('[data-artifact="doomed"]')).toBeVisible();
    await expect(page.locator('.markdown-body h1')).toContainText('Doomed');

    // Remove it out-of-band via the CLI; the `artifact_deleted` event is
    // re-broadcast over SSE and the list updates without a reload.
    await weaver.removeArtifact(session, 'doomed');
    await expect(page.locator('[data-artifact="doomed"]')).toHaveCount(0, {
      timeout: 15_000,
    });
  });

  test('an html artifact renders in a sandboxed iframe, with a source view', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'html report',
      name: 'artifacts-html',
    });
    const html =
      '<!doctype html><html><body><h1 id="hi">Live report</h1>' +
      '<script>document.documentElement.dataset.ran = "1";</script></body></html>';
    await weaver.writeArtifact(session, 'report', html, {
      title: 'Report',
      kind: 'html',
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/report`);

    // Preview is a sandboxed iframe: scripts run, but with no same-origin grant
    // it cannot reach loom's cookies/API as the signed-in user.
    const frame = page.getByTestId('artifact-html');
    await expect(frame).toBeVisible();
    const sandbox = (await frame.getAttribute('sandbox')) ?? '';
    expect(sandbox).toContain('allow-scripts');
    expect(sandbox).not.toContain('allow-same-origin');
    // It's a real document — the markup rendered and its script executed.
    const inner = page.frameLocator('[data-testid="artifact-html"]');
    await expect(inner.locator('#hi')).toHaveText('Live report');
    await expect(inner.locator('html')).toHaveAttribute('data-ran', '1');

    // The Source toggle swaps the live iframe for the raw HTML source.
    await page.getByRole('button', { name: 'Source' }).click();
    await expect(page.locator('pre', { hasText: 'Live report' })).toBeVisible();
    await expect(page.getByTestId('artifact-html')).toHaveCount(0);
  });

  test('a long popped artifact has a bounded scroller and keeps its place when docked', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'side by side',
      name: 'artifacts-pop',
    });
    await weaver.writeArtifact(session, 'notes', longDoc(), { title: 'Notes' });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/notes`);
    await expect(page.locator('.markdown-body h1')).toContainText('Long operational notes');
    // Docked: the artifact fills the work area, so the terminal is hidden.
    await expect(page.locator('[data-term-tab="agent"]')).toBeHidden();

    const dockedScroll = page.getByTestId('artifact-scroll');
    await dockedScroll.evaluate((element) => {
      element.scrollTop = 800;
    });
    const dockedTop = await dockedScroll.evaluate((element) => element.scrollTop);
    expect(dockedTop).toBeGreaterThan(700);

    // Pop out → a rail appears beside the terminal: both visible at once.
    await page.getByTestId('artifact-pop').click();
    await expect(page.getByTestId('artifact-rail-close')).toBeVisible();
    await expect(page.locator('[data-term-tab="agent"]')).toBeVisible(); // terminal back
    await expect(page.locator('.markdown-body h1')).toContainText('Long operational notes');

    // The regression: every flex ancestor is bounded, leaving the document as
    // the scrolling element instead of growing the whole page beyond the
    // viewport. A long document can reach its end inside the popped rail.
    const poppedScroll = page.getByTestId('artifact-scroll');
    const bounds = await poppedScroll.evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      overflowY: getComputedStyle(element).overflowY,
    }));
    expect(bounds.clientHeight).toBeGreaterThan(100);
    expect(bounds.clientHeight).toBeLessThan(await page.evaluate(() => window.innerHeight));
    expect(bounds.scrollHeight).toBeGreaterThan(bounds.clientHeight + 1_000);
    expect(bounds.overflowY).toMatch(/auto|scroll/);
    await expect
      .poll(async () =>
        Math.abs((await poppedScroll.evaluate((element) => element.scrollTop)) - dockedTop),
      )
      .toBeLessThanOrEqual(5);

    await poppedScroll.evaluate((element) => {
      element.scrollTop = 1_600;
    });
    const poppedTop = await poppedScroll.evaluate((element) => element.scrollTop);
    expect(poppedTop).toBeGreaterThan(dockedTop + 500);

    // Dock back → the rail closes and the artifact returns to the work-area tab
    // at the same reading position, rather than jumping to the top.
    await page.getByTestId('artifact-pop').click();
    await expect(page.getByTestId('artifact-rail-close')).toHaveCount(0);
    await expect(page.locator('[data-term-tab="agent"]')).toBeHidden();
    await expect(page.locator('.markdown-body h1')).toContainText('Long operational notes');
    await expect
      .poll(async () =>
        Math.abs(
          (await page.getByTestId('artifact-scroll').evaluate((element) => element.scrollTop)) -
            poppedTop,
        ),
      )
      .toBeLessThanOrEqual(5);
  });

  test('a narrow review keeps the artifact full-width behind session navigation', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'Review an artifact without squeezing the phone layout',
      name: 'mobile-artifact-split',
    });
    await weaver.writeArtifact(session, 'notes', longDoc(), { title: 'Notes' });

    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/notes`);

    // Phones use one full-width work surface: the desktop Split action and
    // permanent sidebar are replaced by one artifact selector.
    await expect(page.getByTestId('artifact-pop')).toBeHidden();
    await expect(page.locator('.session-split-panel')).toHaveCount(0);
    await expect(page.getByTestId('mobile-artifact-select')).toHaveValue('notes');

    const scroll = page.getByTestId('artifact-scroll');
    await expect(scroll).toBeVisible();
    const scrollBox = await scroll.boundingBox();
    expect(scrollBox).not.toBeNull();
    expect(scrollBox!.width).toBeGreaterThan(350);
    expect(
      await scroll.evaluate((element) => element.scrollHeight > element.clientHeight + 1_000),
    ).toBe(true);
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
    ).toBe(true);
    expect(
      await page.evaluate(() => document.documentElement.scrollHeight <= window.innerHeight),
    ).toBe(true);

    const primary = page.getByRole('navigation', { name: 'Primary' });
    const primaryBox = await primary.boundingBox();
    expect(primaryBox).not.toBeNull();
    expect(primaryBox!.width).toBe(390);
    expect(primaryBox!.y).toBeGreaterThan(780);
    await expect(primary.getByRole('button', { name: 'Artifacts' })).toHaveAttribute(
      'aria-current',
      'page',
    );
    await primary.getByRole('button', { name: 'More' }).click();
    await page.getByTestId('mobile-session-changes').click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${session.id}/changes`);
    await expect(primary.getByRole('button', { name: 'More' })).toHaveClass(/text-fg/);
  });

  test('Review routes keep artifacts on the warm session page', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'tabbing',
      name: 'artifacts-tab',
    });
    await weaver.writeArtifact(session, 'plan', '# Plan\n\nthe plan\n', {
      title: 'Plan',
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}`);
    await expect(page.locator('[data-term-tab="agent"]')).toBeVisible();

    // Review opens its canonical Artifacts route without remounting the session.
    await page.getByRole('tab', { name: 'Review' }).click();
    await expect(page).toHaveURL(new RegExp(`/s/${session.id}/artifacts`));
    await expect(page.locator('.markdown-body h1')).toContainText('Plan');
    await expect(page.getByRole('link', { name: 'Changes', exact: true })).toBeVisible();
    await expect(page.getByRole('tab', { name: 'Agent' })).toBeVisible();

    // Back to Agent — the warm terminal returns and the review route closes.
    await page.getByRole('tab', { name: 'Agent' }).click();
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${session.id}`);
    await expect(page.locator('[data-term-tab="agent"]')).toBeVisible();
  });

  test('an image artifact renders directly and embeds into another artifact', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'shots',
      name: 'artifacts-image',
    });
    // A 1×1 transparent PNG, piped as raw bytes (no source extension): the CLI
    // sniffs it from its magic bytes and stores a self-contained image artifact.
    const png = Buffer.from(
      'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
      'base64',
    );
    await weaver.writeArtifact(session, 'A.png', png, { title: 'Screenshot' });
    await weaver.writeArtifact(
      session,
      'B.md',
      '# Results\n\n![Linked screenshot](artifact:A.png)\n',
      { title: 'Results' },
    );

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/A.png`);

    // Standalone image artifacts use the authenticated raw-bytes route.
    const standalone = page.getByTestId('artifact-image');
    await expect(standalone).toHaveAttribute(
      'src',
      new RegExp(`/api/sessions/${session.id}/artifacts/A\\.png/raw\\?rev=1$`),
    );
    await expect(standalone).toHaveAttribute('alt', 'Screenshot');
    await expect
      .poll(() => standalone.evaluate((img: HTMLImageElement) => img.naturalWidth))
      .toBe(1);

    const raw = await page.request.get(
      `${weaver.baseUrl}/api/sessions/${session.id}/artifacts/A.png/raw`,
    );
    expect(raw.ok()).toBe(true);
    expect(raw.headers()['content-type']).toBe('image/png');
    expect(await raw.body()).toEqual(png);

    // Markdown can reuse the stored image by artifact name. The reference is
    // resolved at read time, so no base64 or agent-local path is copied into B.
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/B.md`);
    const linked = page.locator('.markdown-body img');
    await expect(linked).toHaveAttribute(
      'src',
      new RegExp(`/api/sessions/${session.id}/artifacts/A\\.png/raw$`),
    );
    await expect(linked).toHaveAttribute('alt', 'Linked screenshot');
    await expect.poll(() => linked.evaluate((img: HTMLImageElement) => img.naturalWidth)).toBe(1);
  });
});
