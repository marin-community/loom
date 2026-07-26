import { test, expect } from '../fixtures/weaver';

// The embedded editor (code-server) is available from session Details and opens
// in a panel beside the terminal. It no longer occupies permanent edge chrome.
// The proxy/lifecycle is covered by the Rust integration test.
test.describe('embedded editor panel', () => {
  test('opens from Details, then closes without leaving permanent chrome', async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: 'edit some code',
      name: 'ide-panel',
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}`);

    await expect(page.getByTestId('ide-open')).toHaveCount(0);
    await page.getByRole('button', { name: /Details/ }).click();
    await page.getByText('Advanced', { exact: true }).click();
    await page.getByTestId('action-open-editor').click();

    // The panel mounts with its header…
    await expect(page.getByText('Editor', { exact: true })).toBeVisible();
    // …and its body settles into a valid state: the live editor iframe when
    // code-server is installed, else the graceful not-installed note (e.g. CI).
    // Either is correct — neither is a broken frame.
    const liveEditor = page.locator('iframe[title="VS Code"]');
    const notInstalled = page.getByText("code-server isn't installed");
    await expect(liveEditor.or(notInstalled)).toBeVisible();

    // Closing returns to the uncluttered workbench; Details remains the path in.
    await page.getByLabel('Close editor').click();
    await expect(page.getByText('Editor', { exact: true })).toHaveCount(0);
    await expect(page.getByTestId('ide-open')).toHaveCount(0);
    await expect(page.getByRole('button', { name: /Details/ })).toBeVisible();
  });
});
