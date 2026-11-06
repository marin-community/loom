import { test, expect, type Session } from '../fixtures/weaver';

// A profile's `doc_artifact` names the artifact that is its sessions' doc.
const DOC_PROFILE = 'doc-session';
const DOC_ARTIFACT = 'doc';
const JSON_HEADERS = { 'content-type': 'application/json' };

async function launchDocSession(baseUrl: string, repoPath: string): Promise<Session> {
  const existing = await fetch(`${baseUrl}/api/profiles/get`, {
    method: 'POST',
    headers: JSON_HEADERS,
    body: JSON.stringify({ name: DOC_PROFILE }),
  });
  if (!existing.ok) {
    const created = await fetch(`${baseUrl}/api/profiles/create`, {
      method: 'POST',
      headers: JSON_HEADERS,
      body: JSON.stringify({
        name: DOC_PROFILE,
        description: 'Playwright doc sessions',
        agent_kind: 'shell',
        model: '',
        effort: '',
        protocol: 'terminal',
        mode: 'auto',
        class: 'interactive',
        strict: false,
        env_clear: false,
        ambient_allowlist: [],
        max_concurrent: 0,
        prelude: 'weaver',
        doc_artifact: DOC_ARTIFACT,
        restricted: false,
        runtime_permissions: [],
        mcp_access: { mode: 'none', groups: [] },
      }),
    });
    expect(created.ok, await created.text()).toBe(true);
  }
  const launched = await fetch(`${baseUrl}/api/sessions/launch`, {
    method: 'POST',
    headers: JSON_HEADERS,
    body: JSON.stringify({
      goal: 'Keep the design in the doc',
      title: 'doc-session',
      name: 'doc-session',
      cwd: repoPath,
      profile: DOC_PROFILE,
    }),
  });
  expect(launched.ok, await launched.clone().text()).toBe(true);
  return (await launched.json()) as Session;
}

test('a doc session gets a Doc tab that shows only its doc, and opens on it once written', async ({
  page,
  weaver,
}) => {
  const session = await launchDocSession(weaver.baseUrl, weaver.repoPath);
  const sessionUrl = `${weaver.baseUrl}/s/${session.id}`;
  await page.goto(sessionUrl);
  const tabs = page.getByTestId('session-tabs');
  await expect(tabs.locator('[data-tab="terminal"]')).toHaveAttribute('aria-selected', 'true');
  await expect(tabs.locator('[data-tab="document"]')).toHaveCount(0);

  await weaver.writeArtifact(session, 'progress', '# Progress\n', { title: 'Progress' });
  await weaver.writeArtifact(session, DOC_ARTIFACT, '## Objective\n\nShip the Doc tab.\n', {
    title: 'Doc',
  });
  // A doc written while the person is on the page adds the tab without moving them.
  await expect(tabs.locator('[data-tab="document"]')).toBeVisible();
  await expect(tabs.locator('[data-tab="terminal"]')).toHaveAttribute('aria-selected', 'true');

  await page.reload();
  await expect(tabs.locator('[data-tab="document"]')).toHaveAttribute('aria-selected', 'true');
  await expect(page).toHaveURL(sessionUrl);
  await expect(page.locator('.markdown-body h2')).toContainText('Objective');
  await expect(page.locator('[data-artifact]')).toHaveCount(0);
  await expect(page.getByTestId('artifact-delete')).toHaveCount(0);

  await tabs.locator('[data-tab="review"]').click();
  await expect(page).toHaveURL(new RegExp(`/s/${session.id}/artifacts`));
  await expect(page.locator('[data-artifact="progress"]')).toBeVisible();

  await tabs.locator('[data-tab="document"]').click();
  await expect(page).toHaveURL(sessionUrl);
  await expect(tabs.locator('[data-tab="document"]')).toHaveAttribute('aria-selected', 'true');
});
