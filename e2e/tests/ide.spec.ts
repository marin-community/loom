import { test, expect } from '../fixtures/weaver';

// The embedded editor (code-server) lives in a panel pulled in from the right,
// beside the terminal. The proxy/lifecycle is covered by the Rust integration
// test; this drives the UX. CI has no code-server installed, so opening the
// panel must degrade gracefully to a clear "not installed" message — never a
// broken frame.
test.describe('embedded editor panel', () => {
  test('pulls in from the right, then collapses', async ({ page, weaver }) => {
    const session = await weaver.seedSession({ goal: 'edit some code', name: 'ide-panel' });
    await page.goto(`${weaver.baseUrl}/s/${session.id}`);

    // The collapsed edge handle is the "pull from the right" affordance.
    const handle = page.getByTestId('ide-open');
    await expect(handle).toBeVisible();
    await handle.click();

    // The panel mounts with its header…
    await expect(page.getByText('Editor', { exact: true })).toBeVisible();
    // …and, with no code-server on the host, the graceful not-installed note
    // (CI has none — see docs/embedded-ide.md).
    await expect(page.getByText("code-server isn't installed")).toBeVisible();

    // Closing collapses back to the handle.
    await page.getByLabel('Close editor').click();
    await expect(handle).toBeVisible();
  });
});
