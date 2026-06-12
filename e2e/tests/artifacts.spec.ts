import { test, expect } from '../fixtures/weaver';

// Artifacts are the agent's out-of-repo documents: named, scoped (branch vs
// repo-shared), versioned by immutable snapshot, rendered by loom. This drives a
// real `plan` artifact through the running UI — the list, the markdown viewer
// with the smartdoc projection (an `#N` issue ref becomes a live status chip),
// the version picker, an SSE-driven refresh, a user edit that appends a
// revision, and the Overview pin + goal render.

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

test.describe('artifacts surface', () => {
  test('renders the viewer with projection, mermaid, and version history', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({ goal: 'rewrite search', name: 'artifacts-view' });
    const issue = await weaver.seedIssue(session, 'Index layer');

    // Two revisions so the picker has history; the second retitles via the env.
    await weaver.writeArtifact(session, 'plan', planDoc(issue.id, 'v1'), { title: 'Search rewrite' });
    await weaver.writeArtifact(session, 'plan', planDoc(issue.id, 'v2'), { title: 'Search rewrite' });

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
    await expect(page.getByRole('button', { name: 'Edit' })).toBeDisabled();

    // An out-of-band CLI write (rev 3) is re-broadcast over SSE; the viewer,
    // back on latest, refreshes itself. Return to latest first.
    await rev.selectOption('');
    await weaver.writeArtifact(session, 'plan', planDoc(issue.id, 'v3'), { title: 'Search rewrite' });
    await expect(rev.locator('option').first()).toHaveText(/latest \(v3\)/, { timeout: 15_000 });
  });

  test('a user edit in the viewer appends a revision', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'edit me', name: 'artifacts-edit' });
    await weaver.writeArtifact(session, 'notes', '# Notes\n\nFirst draft.\n', { title: 'Notes' });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/notes`);
    await expect(page.locator('.markdown-body h1')).toContainText('Notes');

    // Edit flips the viewer to Monaco; type an addition and save.
    await page.getByRole('button', { name: 'Edit' }).click();
    await expect(page.locator('.monaco-editor')).toBeVisible();
    await page.locator('.monaco-editor').click();
    await page.keyboard.type('\n\nA user revision.');
    await page.getByRole('button', { name: 'Save' }).click();

    // The save appended rev 2 (author: user); the picker and list reflect it.
    await expect(page.getByTestId('artifact-rev').locator('option').first()).toHaveText(
      /latest \(v2\)/,
    );
    await expect(page.locator('[data-artifact="notes"]')).toContainText('v2');
    // Back in preview, the edited content shows.
    await expect(page.locator('.markdown-body')).toContainText('A user revision.');
  });
});

test.describe('overview', () => {
  test('pins the plan artifact and renders the goal as markdown', async ({ page, weaver }) => {
    const session = await weaver.seedSession({
      goal: '# Rewrite search\n\nMake it **fast** and incremental.',
      name: 'artifacts-overview',
    });
    const issue = await weaver.seedIssue(session, 'Index layer');
    await weaver.writeArtifact(session, 'plan', planDoc(issue.id, ''), { title: 'Search rewrite' });

    await page.goto(`${weaver.baseUrl}/s/${session.id}`);
    await page.getByRole('button', { name: 'Overview' }).click();

    // The goal renders through the markdown pipeline (not raw text).
    const goal = page.getByTestId('session-goal');
    await expect(goal.locator('h1')).toContainText('Rewrite search');
    await expect(goal.locator('strong')).toContainText('fast');

    // The well-known `plan` artifact is pinned where the plan used to live, with
    // its title, the projected issue chip, and the architecture diagram.
    const plan = page.getByTestId('session-plan');
    await expect(plan).toContainText('Search rewrite');
    await expect(plan.locator(`a.smartdoc-chip[data-issue="${issue.id}"]`)).toBeVisible();
    await expect(plan.locator('.mermaid-diagram svg').first()).toBeVisible({ timeout: 30_000 });
  });
});
