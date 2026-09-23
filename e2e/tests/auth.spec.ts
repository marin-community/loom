import { test, expect } from '../fixtures/weaver';

// The e2e server binds loopback, so the dashboard loads authenticated as the
// owner via loopback trust (no login step). These cover the Settings → Account
// identity UI end to end against the real API. Token lifecycle and destructive
// confirmation share the narrow cross-feature journey in lifecycle.spec.ts.
test.describe('settings · account identity', () => {
  test('the Account screen shows the loopback identity', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId('settings-category-account').click();
    await expect(page.getByText('Signed in')).toBeVisible();
    // The seeded owner, authenticated via loopback trust.
    await expect(page.getByText('via loopback')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
  });

  test('an admin can create and revoke a deployment-only token', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId('settings-category-account').click();
    await page.getByTestId('deployment-token-name').fill('production');
    await page.getByTestId('deployment-token-create').click();
    await expect(page.getByTestId('deployment-token-secret')).toContainText('loom_');
    await expect(page.getByTestId('deployment-token-row')).toContainText('production');

    await page.getByTestId('deployment-token-revoke').click();
    await page.getByTestId('confirm-dialog-confirm').click();
    await expect(page.getByTestId('deployment-token-row')).toHaveCount(0);
  });
});
