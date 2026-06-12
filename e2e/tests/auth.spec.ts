import { test, expect } from '../fixtures/weaver';

// The e2e server binds loopback, so the dashboard loads authenticated as the
// owner via loopback trust (no login step). These cover the Settings → Tokens
// management UI end to end against the real API.
test.describe('settings · tokens', () => {
  test('creates, lists, and revokes an API token', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);

    // Switch to the Tokens tab.
    await page.getByTestId('settings-tab-tokens').click();
    await expect(page.getByTestId('token-row')).toHaveCount(0);

    // Create a token — the one-time secret banner appears with a `loom_` value.
    await page.getByTestId('token-name').fill('github-actions');
    await page.getByTestId('token-create').click();

    const banner = page.getByTestId('new-token');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('loom_');

    // It now shows in the list.
    const row = page.getByTestId('token-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('github-actions');

    // Revoke it (accept the confirm dialog) and it disappears.
    page.once('dialog', (d) => d.accept());
    await page.getByTestId('token-revoke').click();
    await expect(page.getByTestId('token-row')).toHaveCount(0);
  });

  test('the Account tab shows the loopback identity', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId('settings-tab-account').click();
    await expect(page.getByText('Signed in')).toBeVisible();
    // The seeded owner, authenticated via loopback trust.
    await expect(page.getByText('via loopback')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
  });
});
