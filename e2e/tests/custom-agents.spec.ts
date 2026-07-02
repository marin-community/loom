import { test, expect } from '../fixtures/weaver';

test.describe('settings · custom agents', () => {
  test('add a custom agent, see it in the picker, then delete it', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);

    const panel = page.locator('section').filter({
      has: page.getByRole('heading', { name: 'Custom agents' }),
    });
    await expect(panel).toBeVisible();
    // The e2e fixture defines a command-less `shell` custom agent, so the list is
    // non-empty and shows it as a bare shell.
    const shellRow = panel.locator('li').filter({ hasText: 'shell' });
    await expect(shellRow).toBeVisible();
    await expect(shellRow).toContainText('(bare shell)');

    // Add a new custom agent through the form.
    await panel.getByTestId('custom-agent-add').click();
    await panel.getByTestId('custom-agent-name').fill('aider');
    await panel.getByTestId('custom-agent-label').fill('Aider');
    await panel.getByTestId('custom-agent-launch').fill('aider --message');

    // Capture the Agents settings pane with the add form open, both themes.
    await page.screenshot({ path: '/tmp/custom-agents-dark.png', fullPage: true });
    await page.evaluate(() => {
      localStorage.setItem('loom-theme', 'light');
      document.documentElement.classList.remove('dark');
    });
    await page.screenshot({ path: '/tmp/custom-agents-light.png', fullPage: true });
    await page.evaluate(() => {
      localStorage.setItem('loom-theme', 'dark');
      document.documentElement.classList.add('dark');
    });

    await panel.getByTestId('custom-agent-save').click();

    // It lands in the custom list...
    await expect(panel.locator('li').filter({ hasText: 'Aider' })).toBeVisible();
    // ...and becomes selectable as the session default runtime.
    const session = page.locator('section').filter({
      has: page.getByRole('heading', { name: 'Session default runtime' }),
    });
    await expect(session.getByRole('radio', { name: /Aider/ })).toBeVisible();

    // Delete it (accept the confirm dialog); it leaves the list and the picker.
    page.on('dialog', (d) => d.accept());
    await panel.locator('li').filter({ hasText: 'Aider' }).getByRole('button', { name: 'Delete' }).click();
    await expect(panel.locator('li').filter({ hasText: 'Aider' })).toHaveCount(0);
    await expect(session.getByRole('radio', { name: /Aider/ })).toHaveCount(0);
  });
});
