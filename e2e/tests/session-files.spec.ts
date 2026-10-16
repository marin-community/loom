import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test, expect } from '../fixtures/weaver';

test('file links render reports, resolve siblings, and preserve artifact navigation', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'review files',
    name: 'file-links',
  });
  const directory = join(session.work_dir, 'reports');
  await mkdir(directory);
  const png = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==',
    'base64',
  );
  await writeFile(join(directory, 'plot #1.png'), png);
  await writeFile(
    join(directory, 'index.md'),
    '# Dataset review\n\n![Plot](plot%20%231.png)\n\n[Details](details%20%231.md)\n',
  );
  await writeFile(
    join(directory, 'details #1.md'),
    '# Details\n\nAll rows checked.\n',
  );
  await weaver.writeArtifact(session, 'plan', '# Artifact plan\n');
  const root = `${weaver.baseUrl}/s/${session.id}`;
  await page.goto(`${root}/files/reports/index.md`);
  await expect(page.locator('.markdown-body h1')).toContainText(
    'Dataset review',
  );
  const image = page.getByRole('img', { name: 'Plot', exact: true });
  await expect(image).toBeVisible();
  await expect
    .poll(() => image.evaluate((el: HTMLImageElement) => el.naturalWidth))
    .toBe(1);
  await page.getByRole('link', { name: 'Details', exact: true }).click();
  await expect(page.locator('.markdown-body h1')).toContainText('Details');
  await page.getByRole('button', { name: 'Source', exact: true }).click();
  await expect(page.locator('pre')).toContainText('# Details');
  await writeFile(
    join(directory, 'details #1.md'),
    `# Updated details\n\n[Artifact plan](/s/${session.id}/artifacts/plan)\n`,
  );
  await page.getByRole('button', { name: 'Reload', exact: true }).click();
  await expect(page.locator('.markdown-body h1')).toContainText(
    'Updated details',
  );
  await page.getByRole('link', { name: 'Artifact plan', exact: true }).click();
  await expect(page.locator('.markdown-body h1')).toContainText(
    'Artifact plan',
  );
  await page.goBack();
  await expect(page.locator('.markdown-body h1')).toContainText(
    'Updated details',
  );
  await page.goto(`${root}/files/reports/plot%20%231.png`);
  await expect(page.getByTestId('session-file').getByRole('img')).toBeVisible();
  await page.goto(`${root}/files/missing.md`);
  await expect(page.getByRole('alert')).toContainText('file');
});

test('HTML file scripts stay sandboxed in preview and fullscreen', async ({
  page,
  weaver,
}) => {
  const session = await weaver.seedSession({
    goal: 'html file',
    name: 'file-html',
  });
  await writeFile(
    join(session.work_dir, 'report.html'),
    `<!doctype html><h1>Live report</h1>
    <script>
      try { parent.document.title = 'escaped'; } catch { document.documentElement.dataset.isolated = 'yes'; }
    </script>`,
  );
  await page.goto(`${weaver.baseUrl}/s/${session.id}/files/report.html`);
  const inner = page.frameLocator('[data-testid="document-html"]');
  await expect(inner.getByRole('heading')).toHaveText('Live report');
  await expect(inner.locator('html')).toHaveAttribute('data-isolated', 'yes');
  await page.getByRole('button', { name: 'Fullscreen', exact: false }).click();
  await expect
    .poll(() => page.evaluate(() => document.fullscreenElement !== null))
    .toBe(true);
  await expect(inner.locator('html')).toHaveAttribute('data-isolated', 'yes');
  await page.evaluate(() => document.exitFullscreen());
  await page.getByRole('button', { name: 'Source', exact: true }).click();
  await expect(page.locator('pre')).toContainText('<h1>Live report</h1>');
});
