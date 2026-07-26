import { expect, test } from '../fixtures/weaver';
import { writeFileSync } from 'fs';
import { join } from 'path';

test('reviews a worktree change from the shared Review surface', async ({ page, weaver }) => {
  const session = await weaver.seedSession({
    goal: 'review a small change',
    name: 'changes-review',
  });
  const changedPath = join(session.work_dir, 'review.txt');
  const otherPath = join(session.work_dir, 'other.txt');
  writeFileSync(changedPath, 'first\nsecond\n');
  writeFileSync(otherPath, 'other\n');

  await page.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  await expect(page.getByRole('tab', { name: 'Review' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  await expect(page.getByRole('navigation', { name: 'Review' })).toBeVisible();
  await page.getByRole('button', { name: /review\.txt/ }).click();
  const firstLine = page.getByRole('button', {
    name: /Comment on review\.txt new line 1/,
  });
  await firstLine.focus();
  await page.keyboard.press('Shift+ArrowDown');

  const composer = page.getByTestId('change-comment-composer');
  await expect(firstLine).toBeFocused();
  await expect(composer).toContainText('1–2');
  await composer.locator('textarea').fill('Explain why this line belongs here.');

  writeFileSync(changedPath, 'first\nchanged\nthird\n');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(composer.locator('textarea')).toHaveValue('Explain why this line belongs here.');
  const staleSave = page.waitForResponse(
    (response) =>
      response.status() === 409 &&
      response.request().method() === 'POST' &&
      /\/api\/(?:sessions\/[^/]+\/reviews|reviews\/\d+\/comments)$/.test(response.url()),
  );
  await composer.getByRole('button', { name: 'Add pending comment' }).click();
  await staleSave;
  await expect(composer.locator('textarea')).toHaveValue('Explain why this line belongs here.');

  await page.getByRole('button', { name: /other\.txt/ }).click();
  await page.getByRole('button', { name: /Comment on other\.txt new line 1/ }).click();
  await expect(composer.locator('textarea')).toHaveValue('Explain why this line belongs here.');
  await page.getByRole('button', { name: /Comment on review\.txt new line 2/ }).click();
  await expect(composer.locator('textarea')).toHaveValue('Explain why this line belongs here.');
  await composer.getByRole('button', { name: 'Add pending comment' }).click();

  const tray = page.getByTestId('review-tray');
  await expect(tray).toContainText('1 pending');
  await expect(tray.locator('pre')).toContainText('Explain why this line belongs here.');

  writeFileSync(changedPath, 'first\nchanged\nthird\nfourth\n');
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

  const overall = page.getByTestId('review-overall-note');
  const initialSave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'PATCH' &&
      /\/api\/reviews\/\d+$/.test(response.url()),
  );
  await overall.fill('Shared overall note.');
  await overall.press('Tab');
  await initialSave;

  const peer = await page.context().newPage();
  await peer.goto(`${weaver.baseUrl}/s/${session.id}/changes`);
  await peer.getByTestId('review-tray-toggle').click();
  await overall.fill('Keep this local edit after the conflict.');
  const peerSubmit = peer.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'POST' &&
      /\/api\/reviews\/\d+\/submit$/.test(response.url()),
  );
  await peer.getByTestId('submit-review').click();
  await peerSubmit;

  const saveConflict = page.waitForResponse(
    (response) =>
      response.status() === 409 &&
      response.request().method() === 'PATCH' &&
      /\/api\/reviews\/\d+$/.test(response.url()),
  );
  await overall.focus();
  await overall.press('Tab');
  await saveConflict;
  await expect(overall).toHaveValue('Keep this local edit after the conflict.');

  const retrySave = page.waitForResponse(
    (response) =>
      response.ok() &&
      response.request().method() === 'PATCH' &&
      /\/api\/reviews\/\d+$/.test(response.url()),
  );
  await overall.focus();
  await overall.press('Tab');
  await retrySave;
  await expect(tray).toContainText('0 pending');
  await peer.close();

  await page.setViewportSize({ width: 390, height: 720 });
  await expect(page.getByRole('link', { name: 'Artifacts', exact: true })).toBeVisible();
  await expect(page.getByRole('link', { name: 'Changes', exact: true })).toBeVisible();
  await page.getByRole('button', { name: /Details/ }).click();
  await page.getByText('Advanced', { exact: true }).click();
  await expect(page.getByTestId('action-open-editor')).toBeVisible();
});
