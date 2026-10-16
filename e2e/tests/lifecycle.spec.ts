import { test, expect } from '../fixtures/weaver';

test.describe('session lifecycle actions', () => {
  test('narrow keyboard flow confirms destructive account and session actions', async ({
    page,
    weaver,
  }) => {
    await page.setViewportSize({ width: 390, height: 620 });
    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId('settings-category-account').click();
    await page.getByTestId('token-name').fill('narrow-flow');
    await page.getByTestId('token-create').click();
    const revoke = page.getByTestId('token-revoke');

    await revoke.click();
    const dialog = page.getByTestId('confirm-dialog');
    const cancel = page.getByTestId('confirm-dialog-cancel');
    const confirm = page.getByTestId('confirm-dialog-confirm');
    await expect(dialog).toContainText('Revoke API token "narrow-flow"?');
    await expect(dialog).toContainText('Destructive action');
    await expect(cancel).toBeFocused();
    await page.keyboard.press('Shift+Tab');
    await expect(confirm).toBeFocused();
    await page.keyboard.press('Tab');
    await expect(cancel).toBeFocused();
    await page.keyboard.press('Escape');
    await expect(dialog).toHaveCount(0);
    await expect(revoke).toBeFocused();

    let releaseFailure = () => {};
    let injectFailure = true;
    await page.route('**/api/auth/tokens/revoke', async (route) => {
      if (!injectFailure) {
        await route.continue();
        return;
      }
      injectFailure = false;
      await new Promise<void>((resolve) => {
        releaseFailure = resolve;
      });
      await route.fulfill({
        status: 500,
        contentType: 'application/json',
        body: JSON.stringify({ error: 'injected revoke failure' }),
      });
    });
    await revoke.click();
    await confirm.click();
    await expect(confirm).toHaveText('Working…');
    await expect(confirm).toBeDisabled();
    releaseFailure();
    await expect(dialog.getByRole('alert')).toContainText('injected revoke failure');
    await expect(confirm).toHaveText('Revoke token');
    await page.keyboard.press('Escape');
    await expect(revoke).toBeFocused();
    await revoke.click();
    await confirm.click();
    await expect(page.getByTestId('token-row')).toHaveCount(0);

    // The session page must publish its mobile navigation before the More
    // sheet offers its session actions; under load the clicks can beat the
    // publication, so wait for the session-contextual nav (its back link
    // carries data-mobile-nav) before opening it.
    const openMobileSessionDetails = async () => {
      await expect(page.locator('[data-mobile-nav="sessions"]')).toBeVisible();
      await page.getByRole('button', { name: 'More' }).click();
      await page.getByRole('button', { name: 'Details & actions' }).click();
    };

    const archived = await weaver.seedSession({
      goal: 'Archive this work without losing its history',
      name: 'archive-task',
    });
    await page.goto(`${weaver.baseUrl}/s/${archived.id}`);
    await openMobileSessionDetails();
    await page.getByTestId('action-archive').scrollIntoViewIfNeeded();
    await page.getByTestId('action-archive').click();
    await expect(dialog).toContainText('branch, conversation, placement, and Loom history');
    const archivedResponse = page.waitForResponse(
      (response) =>
        response.ok() &&
        response.request().method() === 'POST' &&
        new URL(response.url()).pathname === '/api/sessions/archive' &&
        (response.request().postDataJSON() as { session?: string })?.session === archived.id,
    );
    await confirm.click();
    await archivedResponse;
    await expect(page.getByTestId('status-badge')).toHaveText(/archived/i);
    expect((await weaver.getSession(archived.id)).status).toBe('archived');

    const removed = await weaver.seedSession({
      goal: Array.from({ length: 30 }, (_, index) => `Goal context line ${index + 1}`).join('\n'),
      name: 'remove-task',
    });
    await page.goto(`${weaver.baseUrl}/s/${removed.id}`);
    await openMobileSessionDetails();
    const scroller = page.getByTestId('details-scroll');
    await expect(scroller).toHaveCSS('overflow-y', 'auto');
    const remove = page.getByTestId('action-remove');
    await remove.scrollIntoViewIfNeeded();
    await remove.click();
    await expect(dialog).toContainText('Git branch, conversation, and Loom history');
    await page.goBack();
    await expect(dialog).toHaveCount(0);
    expect((await weaver.getSession(removed.id)).id).toBe(removed.id);
    await page.goForward();
    if (!(await remove.isVisible())) {
      await openMobileSessionDetails();
    }
    await remove.scrollIntoViewIfNeeded();
    await remove.click();
    await page.keyboard.press('Escape');
    await expect(remove).toBeFocused();
    await remove.click();
    await confirm.click();
    await expect(page).toHaveURL(/\/\?space=space-user$/);
    await expect(page.locator(`[data-session-id="${removed.id}"]`)).toHaveCount(0);
  });

  test('a session can opt out of automatic archive from Details', async ({ page, weaver }) => {
    const s = await weaver.seedSession({
      goal: 'Keep me live',
      name: 'no-auto-archive',
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByTestId('action-auto-archive').click();

    await expect(page.getByTestId('tag-pill')).toContainText('auto-archive: disabled');
    await expect.poll(async () => (await weaver.getSession(s.id)).branch.tags).toContainEqual(
      expect.objectContaining({ key: 'auto-archive', value: 'disabled' }),
    );

    await expect(page.getByTestId('action-auto-archive')).toContainText('Enable auto-archive');
    await page.getByTestId('action-auto-archive').click();

    await expect(page.getByTestId('tag-pill')).toHaveCount(0);
    await expect
      .poll(async () => (await weaver.getSession(s.id)).branch.tags)
      .not.toContainEqual(expect.objectContaining({ key: 'auto-archive' }));
  });

  test('archiving a parent can take its children along', async ({ page, weaver }) => {
    const parent = await weaver.seedSession({ goal: 'Delegate the work', name: 'parent-task' });
    const child = await weaver.seedSession({
      goal: 'Do the delegated work',
      name: 'child-task',
      parent: parent.branch.id,
    });

    await page.goto(`${weaver.baseUrl}/s/${parent.id}`);
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByTestId('action-archive').click();
    const dialog = page.getByTestId('confirm-dialog');
    await expect(dialog).toContainText('Archive this session?');
    await expect(dialog.getByTestId('confirm-dialog-option')).toContainText(
      'Also archive 1 child session',
    );
    await dialog.getByTestId('confirm-dialog-option').getByRole('checkbox').check();
    await page.getByTestId('confirm-dialog-confirm').click();
    await expect.poll(async () => (await weaver.getSession(parent.id)).status).toBe('archived');
    await expect.poll(async () => (await weaver.getSession(child.id)).status).toBe('archived');
  });

  test('archiving a parent leaves the subtree alone unless asked', async ({ page, weaver }) => {
    const parent = await weaver.seedSession({ goal: 'Delegate only myself', name: 'solo-parent' });
    const child = await weaver.seedSession({
      goal: 'Survive the parent archive',
      name: 'surviving-child',
      parent: parent.branch.id,
    });

    await page.goto(`${weaver.baseUrl}/s/${parent.id}`);
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByTestId('action-archive').click();
    const dialog = page.getByTestId('confirm-dialog');
    await expect(dialog.getByTestId('confirm-dialog-option')).toContainText(
      'Also archive 1 child session',
    );
    await page.getByTestId('confirm-dialog-confirm').click();
    await expect.poll(async () => (await weaver.getSession(parent.id)).status).toBe('archived');
    expect((await weaver.getSession(child.id)).status).not.toBe('archived');
  });

  test('bulk archive from the selection toolbar can include children', async ({ page, weaver }) => {
    const parent = await weaver.seedSession({ goal: 'Fleet parent', name: 'fleet-parent' });
    const child = await weaver.seedSession({
      goal: 'Fleet child',
      name: 'fleet-child',
      parent: parent.branch.id,
    });

    await page.goto(weaver.baseUrl);
    await page.locator(`[data-session-id="${parent.id}"]`).getByRole('checkbox').check();
    await page.getByTestId('selection-toolbar').getByRole('button', { name: 'Archive' }).click();
    const dialog = page.getByTestId('confirm-dialog');
    await expect(dialog).toContainText('Archive 1 selected session?');
    await expect(dialog.getByTestId('confirm-dialog-option')).toContainText(
      'Also archive 1 child session',
    );
    await dialog.getByTestId('confirm-dialog-option').getByRole('checkbox').check();
    await page.getByTestId('confirm-dialog-confirm').click();
    await expect.poll(async () => (await weaver.getSession(parent.id)).status).toBe('archived');
    await expect.poll(async () => (await weaver.getSession(child.id)).status).toBe('archived');
  });
});
