import { test, expect } from '../fixtures/weaver';

// The e2e server binds loopback, so the dashboard loads authenticated as the
// owner via loopback trust (no login step). These cover the Settings → Tokens
// identity UI end to end against the real API. Token lifecycle and destructive
// confirmation share the narrow cross-feature journey in lifecycle.spec.ts.
test.describe('settings · tokens', () => {
  test('the Access screen shows the loopback identity', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId('settings-tab-access').click();
    await expect(page.getByText('Signed in')).toBeVisible();
    // The seeded owner, authenticated via loopback trust.
    await expect(page.getByText('via loopback')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible();
  });
});
