import { expect, test } from '../fixtures/weaver';
import { writeFileSync } from 'fs';
import { join } from 'path';

test('reviews a worktree change from the shared Review surface', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'review a small change',
    name: 'changes-review',
  });
  const changedPath = join(session.work_dir, 'review.txt');
  writeFileSync(changedPath, 'first\nsecond\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  await expect(page.getByRole('tab', { name: 'Review' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  await expect(page.getByRole('navigation', { name: 'Review' })).toBeVisible();
  await page.getByRole('button', { name: /review\.txt/ }).click();
  await page.getByRole('button', { name: /Comment on review\.txt new line 2/ }).click();

  const composer = page.getByTestId('change-comment-composer');
  await expect(composer.locator('textarea')).toBeFocused();
  await composer.locator('textarea').fill('Explain why this line belongs here.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();

  const tray = page.getByTestId('review-tray');
  await expect(tray).toContainText('1 pending');
  await expect(tray.locator('pre')).toContainText('Explain why this line belongs here.');

  writeFileSync(changedPath, 'first\nsecond\nthird\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.getByTestId('review-stale-warning')).toBeVisible();
  await page.getByTestId('review-stale-ack').check();
  const submit = page.waitForRequest(
    (request) =>
      request.method() === 'POST' && /\/api\/reviews\/\d+\/submit$/.test(request.url()),
  );
  await page.getByTestId('submit-review').click();
  await submit;
  await expect(tray).toContainText('Review submitted');

  await page.setViewportSize({ width: 390, height: 720 });
  await expect(page.getByRole('link', { name: 'Artifacts', exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Changes', exact: true })).toBeVisible();
  await page.getByRole('button', { name: /Details/ }).click();
  await page.getByText('Advanced', { exact: true }).click();
  await expect(page.getByTestId('action-open-editor')).toBeVisible();
});
