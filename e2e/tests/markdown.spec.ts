import { test, expect } from '../fixtures/weaver';
import { writeFileSync } from 'fs';
import { join } from 'path';

// The file browser's Preview mode renders Markdown the way GitHub does:
// GFM tables/task-lists, syntax-highlighted code, inline images resolved
// against the worktree, and mermaid diagrams. This drives a real doc through
// the running UI and asserts each of those surfaces.
test.describe('rich markdown preview', () => {
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

    // Drop the doc and the image it references straight into the worktree;
    // both show up in the (untracked-aware) file tree.
    writeFileSync(join(session.work_dir, 'DESIGN.md'), DOC);
    writeFileSync(join(session.work_dir, 'shot.png'), Buffer.from([0x89, 0x50, 0x4e, 0x47]));

    await page.goto(`${weaver.baseUrl}/#/s/${session.id}/files`);

    // Open the doc — markdown files default to the rendered Preview.
    await page.getByText('DESIGN.md', { exact: true }).first().click();

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
    await expect(body.locator('a', { hasText: 'external link' })).toHaveAttribute('target', '_blank');

    // The relative image resolves to the session's raw-bytes endpoint.
    await expect(body.locator('img')).toHaveAttribute(
      'src',
      new RegExp(`/api/sessions/${session.id}/raw\\?path=shot\\.png$`),
    );

    // The mermaid block is rendered to an SVG (its bundle is lazy-loaded).
    await expect(body.locator('.mermaid-diagram svg')).toBeVisible();

    // Source flips to the Monaco editor showing the raw markdown…
    await page.getByRole('button', { name: 'Source', exact: true }).click();
    await expect(page.locator('.monaco-editor')).toBeVisible();
    await expect(body).toBeHidden();

    // …and Preview flips back to the rendered view.
    await page.getByRole('button', { name: 'Preview', exact: true }).click();
    await expect(page.locator('.markdown-body h1')).toContainText('Design Doc');
  });

  test('non-markdown files have no Preview toggle', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'code', name: 'no-preview' });
    writeFileSync(join(session.work_dir, 'main.rs'), 'fn main() {}\n');

    await page.goto(`${weaver.baseUrl}/#/s/${session.id}/files`);
    await page.getByText('main.rs', { exact: true }).first().click();

    // Source code opens in Monaco; non-markdown gets neither a Preview button
    // nor the rendered markdown surface.
    await expect(page.locator('.monaco-editor.modified-in-monaco-diff-editor')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Preview', exact: true })).toHaveCount(0);
    await expect(page.locator('.markdown-body')).toHaveCount(0);
  });
});
