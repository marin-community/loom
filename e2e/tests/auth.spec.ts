import { test, expect } from '../fixtures/weaver';

// The e2e server binds loopback, so the dashboard loads authenticated as the
// owner via loopback trust (no login step). These cover the Settings → Tokens
// management UI end to end against the real API.
test.describe('settings · tokens', () => {
  test('creates, lists, and revokes an API token', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);

    // Tokens and account management share the consolidated Access screen.
    await page.getByTestId('settings-tab-access').click();
    await expect(page.getByTestId('token-row')).toHaveCount(0);

    // Personal tokens default to the recommended 30-day lifetime.
    await expect(page.getByTestId('token-lifetime')).toHaveValue('30');
    await page.getByTestId('token-name').fill('laptop');
    await page.getByTestId('token-create').click();

    const banner = page.getByTestId('new-token');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText('loom_');

    // It now shows in the list.
    const row = page.getByTestId('token-row');
    await expect(row).toHaveCount(1);
    await expect(row).toContainText('laptop');
    await expect(row).toContainText('Expires');

    // Revoke it (accept the confirm dialog) and it disappears.
    page.once('dialog', (d) => d.accept());
    await page.getByTestId('token-revoke').click();
    await expect(page.getByTestId('token-row')).toHaveCount(0);
  });

  test('the Access screen shows the loopback identity', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId('settings-tab-access').click();
    await expect(page.getByText('Signed in')).toBeVisible();
    // The seeded owner, authenticated via loopback trust.
    await expect(page.getByText('via loopback')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
  });
});
