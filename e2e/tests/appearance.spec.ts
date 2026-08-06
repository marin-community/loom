import { test, expect } from '../fixtures/weaver';

test.describe('settings · terminal appearance', () => {
  test('live preview, save, persist, and reset', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings?tab=workspace`);
    const panel = page.getByTestId('appearance-panel');

    // The live preview is a real xterm instance — the same renderer a session
    // terminal uses — so it proves the config resolves end to end.
    const preview = page.getByTestId('appearance-preview');
    await expect(preview.locator('.xterm')).toBeVisible();

    // Defaults: dark theme, IBM Plex Mono, 13px.
    await expect(page.getByTestId('theme-dark')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('font-plex')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('font-size-input')).toHaveValue('13');

    // Change every knob and save.
    await page.getByTestId('theme-light').click();
    await page.getByTestId('font-jetbrains').click();
    await page.getByTestId('font-size-input').fill('16');
    await expect(page.getByTestId('theme-light')).toHaveAttribute('aria-pressed', 'true');
    await panel.getByRole('button', { name: 'Save' }).click();
    await expect(panel.getByText('Saved terminal appearance.')).toBeVisible();

    // The server is the source of truth — the API reflects the new values.
    const settings = (await page.evaluate(async () => {
      const r = await fetch('/api/settings');
      return (await r.json()).settings as { key: string; value: string }[];
    })) as { key: string; value: string }[];
    const value = (k: string) => settings.find((s) => s.key === k)?.value;
    expect(value('terminal.theme')).toBe('light');
    expect(value('terminal.font')).toBe('jetbrains');
    expect(value('terminal.font_size')).toBe('16');

    // …and the selection survives a reload (the panel reads back from /settings).
    await page.reload();
    await expect(page.getByTestId('theme-light')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('font-jetbrains')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('font-size-input')).toHaveValue('16');

    // Clearing overrides returns all three to their inherited values.
    await panel.getByRole('button', { name: 'Clear overrides' }).click();
    await expect(panel.getByText('Cleared terminal appearance overrides.')).toBeVisible();
    await expect(page.getByTestId('theme-dark')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('font-plex')).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByTestId('font-size-input')).toHaveValue('13');
  });
});
