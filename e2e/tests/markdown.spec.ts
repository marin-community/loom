import { test, expect } from '../fixtures/weaver';
import { writeFileSync } from 'fs';
import { join } from 'path';

// loom's Markdown rendering pipeline (MarkdownView + markdown.ts): GFM
// tables/task-lists, syntax-highlighted code, inline images resolved against
// the worktree, and mermaid diagrams. It is exercised here through the Artifacts
// viewer, which hosts it (the bespoke Files browser that used to host it is
// gone — the embedded editor is the file surface now). This drives a real markdown artifact
// through the running UI and asserts each surface.
test.describe('rich markdown rendering', () => {
  const DOC = [
    '# Design Doc',
    '',
    'Some **bold** text and an [external link](https://example.com).',
    '',
    '| Col A | Col B |',
    '| ----- | ----- |',
    '| one   | two   |',
    '',
    '- [x] shipped',
    '- [ ] pending',
    '',
    '```js',
    'const answer = 42;',
    '```',
    '',
    '![a screenshot](shot.png)',
    '',
    '```mermaid',
    'graph TD; A-->B;',
    '```',
    '',
  ].join('\n');

  test('renders GFM, code, an inline image, and a mermaid diagram', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'write docs', name: 'md-preview' });

    // The image the doc references lives in the worktree; inline images resolve
    // against the session's raw-bytes endpoint regardless of the hosting view.
    writeFileSync(join(session.work_dir, 'shot.png'), Buffer.from([0x89, 0x50, 0x4e, 0x47]));
    await weaver.writeArtifact(session, 'design', DOC, { title: 'Design Doc' });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);

    // Markdown artifacts open in the rendered Preview.
    const body = page.locator('.markdown-body');
    await expect(body.locator('h1')).toContainText('Design Doc');
    // The heading is slugged and carries a GitHub-style permalink.
    await expect(body.locator('h1')).toHaveAttribute('id', 'design-doc');
    await expect(body.locator('h1 a.header-anchor')).toHaveAttribute('href', '#design-doc');
    await expect(body.locator('table')).toBeVisible();
    await expect(body.locator('td')).toHaveCount(2);

    // GitHub task lists become disabled checkboxes, one checked.
    const boxes = body.locator('input[type=checkbox]');
    await expect(boxes).toHaveCount(2);
    await expect(boxes.nth(0)).toBeChecked();
    await expect(boxes.nth(1)).not.toBeChecked();

    // Fenced code is syntax-highlighted.
    await expect(body.locator('pre.hljs')).toBeVisible();

    // External links open in a new tab.
    await expect(body.locator('a', { hasText: 'external link' })).toHaveAttribute(
      'target',
      '_blank',
    );

    // The relative image resolves to the session's raw-bytes endpoint.
    await expect(body.locator('img')).toHaveAttribute(
      'src',
      new RegExp(`/api/sessions/${session.id}/raw\\?path=shot\\.png$`),
    );

    // The mermaid block is rendered to an SVG. Its bundle is lazy-loaded and the
    // render is async, so allow extra time and wait on actual rendered geometry
    // (a <g> node inside the svg) rather than first-paint visibility of a still-
    // empty <svg>, which flakes when the machine is loaded by parallel workers.
    const mermaidSvg = body.locator('.mermaid-diagram svg');
    await expect(mermaidSvg).toBeVisible({ timeout: 30_000 });
    await expect(mermaidSvg.locator('g').first()).toBeVisible({ timeout: 30_000 });
    // Node labels must survive rendering. Mermaid draws them as HTML inside
    // `<foreignObject>`; our DOMPurify pass has to keep that intact (it once
    // stripped it, leaving every box blank), so assert the label text is there.
    await expect(mermaidSvg.locator('.nodeLabel').first()).toHaveText('A');

    // Source flips to the Monaco editor showing the raw markdown…
    await page.getByRole('button', { name: 'Source', exact: true }).click();
    await expect(page.locator('.monaco-editor')).toBeVisible();
    await expect(body).toBeHidden();

    // …and Preview flips back to the rendered view.
    await page.getByRole('button', { name: 'Preview', exact: true }).click();
    await expect(page.locator('.markdown-body h1')).toContainText('Design Doc');
  });

  test('a broken mermaid diagram errors inline, never leaking into the page body', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({ goal: 'write docs', name: 'md-bad-mermaid' });
    // A diagram mermaid can't parse. By default mermaid draws its "bomb" error
    // graphic into a temp node it appends to `document.body` and only removes on
    // success — so a failed render used to leak the bomb into the page body,
    // where it survived route changes and stacked up at the bottom of every view.
    const doc = ['# Doc', '', '```mermaid', 'graph TD; A --> ((( broken', '```', ''].join('\n');
    await weaver.writeArtifact(session, 'bad-diagram', doc, { title: 'Bad diagram' });

    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/bad-diagram`);

    // The failure surfaces as our own inline note inside the preview…
    await expect(page.locator('.markdown-body .mermaid-error')).toBeVisible({ timeout: 30_000 });

    // …and nothing leaks into the document body outside the app.
    const leaked = await page.evaluate(
      () =>
        document.querySelectorAll(
          'body > svg[aria-roledescription="error"], body > .mermaid, body .error-icon',
        ).length,
    );
    expect(leaked).toBe(0);
  });
});
