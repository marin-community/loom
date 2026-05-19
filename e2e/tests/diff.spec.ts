import { test, expect } from '../fixtures/weaver';
import { writeFileSync } from 'fs';
import { join } from 'path';

test.describe('workspace diff', () => {
  test('Load diff shows a newly created file in the worktree', async ({ page, weaver }) => {
    const ws = await weaver.seedWorkspace({ goal: 'Produce a diff', name: 'diff-task' });

    // The diff endpoint includes untracked files; drop one into the worktree.
    writeFileSync(
      join(ws.work_dir, 'NEW_FILE.txt'),
      'this file was added by the e2e diff test\n',
    );

    await page.goto(`${weaver.baseUrl}/#/w/${ws.id}`);

    await page.getByRole('button', { name: 'Load diff' }).click();

    // Diff section renders a stats line and the patch including the new file.
    const diffPre = page.locator('pre').last();
    await expect(diffPre).toContainText('NEW_FILE.txt');
    await expect(diffPre).toContainText('this file was added by the e2e diff test');
    await expect(page.getByText(/files ·/)).toBeVisible();
  });
});
