import { test, expect } from '../fixtures/weaver';

test.describe('settings · agent defaults', () => {
  test('agent settings use registry-backed model and effort choices', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);

    const session = page.locator('section').filter({
      has: page.getByRole('heading', { name: 'Session default runtime' }),
    });
    const concierge = page.locator('section').filter({
      has: page.getByRole('heading', { name: 'Fleet concierge runtime' }),
    });

    await expect(session.getByRole('radio', { name: /Claude/ })).toBeVisible();
    await expect(session.getByRole('radio', { name: /Codex/ })).toBeVisible();
    await expect(session.getByRole('radio', { name: /Shell/ })).toBeVisible();

    await session.getByRole('radio', { name: /Codex/ }).click();
    await expect(session.getByRole('button', { name: 'GPT-5.5' })).toBeVisible();
    await expect(session.getByRole('button', { name: 'GPT-5.4', exact: true })).toBeVisible();
    await expect(session.getByRole('button', { name: 'GPT-5.4 Mini' })).toBeVisible();
    await expect(session.getByRole('button', { name: 'GPT-5.3 Codex Spark' })).toBeVisible();
    await expect(session.getByRole('button', { name: 'Haiku' })).toHaveCount(0);
    await expect(session.getByRole('button', { name: 'Max' })).toHaveCount(0);

    await session.getByRole('radio', { name: /Claude/ }).click();
    await expect(session.getByRole('button', { name: 'Haiku' })).toBeVisible();
    await expect(session.getByRole('button', { name: 'Sonnet' })).toBeVisible();
    await expect(session.getByRole('button', { name: 'Opus' })).toBeVisible();
    await expect(session.getByRole('button', { name: 'Fable' })).toBeVisible();
    await expect(session.getByRole('button', { name: 'Max' })).toBeVisible();

    await expect(concierge.getByRole('radio', { name: /Claude/ })).toBeVisible();
    await expect(concierge.getByRole('radio', { name: /Codex/ })).toBeVisible();
    await expect(concierge.getByRole('radio', { name: /Shell/ })).toHaveCount(0);

    await concierge.getByRole('radio', { name: /Codex/ }).click();
    await expect(concierge.getByRole('button', { name: 'GPT-5.5' })).toBeVisible();
    await expect(concierge.getByRole('button', { name: 'GPT-5.4', exact: true })).toBeVisible();
    await expect(concierge.getByRole('button', { name: 'GPT-5.4 Mini' })).toBeVisible();
    await expect(concierge.getByRole('button', { name: 'GPT-5.3 Codex Spark' })).toBeVisible();
    await expect(concierge.getByRole('button', { name: 'Max' })).toHaveCount(0);
  });
});
