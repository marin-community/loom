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

    await expect(page.getByText('Fleet concierge runtime', { exact: true })).toHaveCount(0);
  });

  test('default agent permissions can be set to always allow', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    const session = page.locator('section').filter({
      has: page.getByRole('heading', { name: 'Session default runtime' }),
    });
    const permissions = session.getByTestId('agent-mode-picker');

    await expect(permissions.getByRole('button', { name: /Auto/ })).toHaveAttribute(
      'data-active',
      'true',
    );
    await permissions.getByRole('button', { name: /Always allow/ }).click();
    await session.getByRole('button', { name: 'Save' }).click();
    await expect(page.getByText('Saved Session default runtime.')).toBeVisible();

    const settings = (await (await fetch(`${weaver.baseUrl}/api/settings`)).json()) as {
      settings: { key: string; value: string; is_default: boolean }[];
    };
    expect(settings.settings.find((setting) => setting.key === 'agent.mode')).toMatchObject({
      value: 'bypassPermissions',
      is_default: false,
    });
    await expect(permissions.getByRole('button', { name: /Always allow/ })).toHaveAttribute(
      'data-active',
      'true',
    );
  });

  test('overlapping settings are consolidated into workspace and access', async ({ page, weaver }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await expect(page.getByRole('button', { name: 'Workspace', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Access', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Editor', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Appearance', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Authentication', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Tokens', exact: true })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Account', exact: true })).toHaveCount(0);
    await expect(page.locator('[data-rail="chat"]')).toHaveCount(0);
  });
});
