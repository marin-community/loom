import { test, expect } from '../fixtures/weaver';

// The Overview tab is the session BRIEF — the catch-up pane. These cover the
// three sections this rework added (state synopsis, document list, surfaced
// links) and the list row's side-door link that lands on it.

test.describe('session brief', () => {
  test('the fleet row offers an overview side door that lands on the brief', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ name: 'brief-door', goal: 'Ship the brief' });
    await weaver.setStatus(s, 'ok', 'mapping the code');

    await page.goto(weaver.baseUrl);
    const row = page.getByTestId('session-card').filter({ hasText: 'brief-door' });
    await row.getByTestId('row-overview').click();

    await expect(page).toHaveURL(new RegExp(`/s/${s.id}\\?tab=overview`));
    // The brief leads with the state synopsis — the agent's last message.
    await expect(page.getByTestId('session-state')).toContainText('mapping the code');
  });

  test('the brief shows state, documents, and surfaced links', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ name: 'brief-full', goal: 'Ship the brief' });
    await weaver.writeArtifact(s, 'design', '# The design\n', { title: 'The design' });
    await weaver.setTag(s, 'github', 'acme/widgets#87');
    await weaver.setStatus(s, 'attention', 'draft at https://example.test/doc ready for eyes');

    await page.goto(`${weaver.baseUrl}/s/${s.id}?tab=overview`);

    // State: the message, and the loud level called out beneath it.
    const state = page.getByTestId('session-state');
    await expect(state).toContainText('draft at https://example.test/doc ready for eyes');
    await expect(state).toContainText('attention');

    // Documents: the artifact by name + title, linking into the viewer.
    const docs = page.getByTestId('session-docs');
    await expect(docs).toContainText('design');
    await expect(docs).toContainText('The design');
    await expect(docs.getByRole('link', { name: /design/ })).toHaveAttribute(
      'href',
      `/s/${s.id}/artifacts/design`,
    );

    // Links: the wired GitHub thread first, then the URL harvested from the
    // status trail.
    const links = page.getByTestId('session-links');
    await expect(links).toContainText('acme/widgets#87');
    await expect(links.getByRole('link', { name: 'acme/widgets#87' })).toHaveAttribute(
      'href',
      'https://github.com/acme/widgets/issues/87',
    );
    await expect(links).toContainText('example.test/doc');
  });

  test('a calm session with nothing published shows the quiet empty brief', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ name: 'brief-empty', goal: 'Quiet start' });

    await page.goto(`${weaver.baseUrl}/s/${s.id}?tab=overview`);

    await expect(page.getByTestId('session-state')).toContainText('No status reported yet.');
    // No documents, no links — the sections stay absent, not empty husks.
    await expect(page.getByTestId('session-docs')).toHaveCount(0);
    await expect(page.getByTestId('session-links')).toHaveCount(0);
  });
});
