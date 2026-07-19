import { test, expect } from '../fixtures/weaver';

test.describe('settings · agent defaults', () => {
  test('agent settings use registry-backed model and effort choices', async ({ page, weaver }) => {
    const registry = (await (await fetch(`${weaver.baseUrl}/api/agents`)).json()) as {
      agents: { kind: string; models: { label: string }[]; efforts: { label: string }[] }[];
    };
    const codex = registry.agents.find((agent) => agent.kind === 'codex')!;
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
    for (const model of codex.models) {
      await expect(session.getByRole('button', { name: model.label, exact: true })).toBeVisible();
    }
    for (const effort of codex.efforts) {
      await expect(session.getByRole('button', { name: effort.label, exact: true })).toBeVisible();
    }
    await expect(session.getByRole('button', { name: 'Haiku' })).toHaveCount(0);

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
    for (const model of codex.models) {
      await expect(concierge.getByRole('button', { name: model.label, exact: true })).toBeVisible();
    }
    for (const effort of codex.efforts) {
      await expect(concierge.getByRole('button', { name: effort.label, exact: true })).toBeVisible();
    }
  });
});
