import { test, expect } from '../fixtures/weaver';

// Covers the app-icon + UI-cleanup work: a served favicon, consistent
// "Weaver - <Section>" tab titles, session rows that are real links (right-click
// / middle-click / ⌘-click open in a new tab), and ⌘/Ctrl+Enter submitting the
// New Session form.

test.describe('app icon (favicon)', () => {
  test('serves the SVG icon and links it from the document head', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);

    // The head links the vector icon + PNG fallbacks + apple-touch icon.
    await expect(page.locator('link[rel="icon"][type="image/svg+xml"]')).toHaveAttribute(
      'href',
      '/favicon.svg',
    );
    await expect(page.locator('link[rel="apple-touch-icon"]')).toHaveCount(1);

    // The SVG asset is actually served (not a 404 SPA fallback to index.html).
    const res = await page.request.get(`${weaver.baseUrl}/favicon.svg`);
    expect(res.ok()).toBeTruthy();
    expect(res.headers()['content-type']).toContain('image/svg+xml');
    expect(await res.text()).toContain('<svg');

    // And the PNG fallback resolves too.
    const png = await page.request.get(`${weaver.baseUrl}/favicon-32.png`);
    expect(png.ok()).toBeTruthy();
    expect(png.headers()['content-type']).toContain('image/png');
  });
});

test.describe('consistent page titles', () => {
  test('each route sets a "Weaver - <Section>" tab title', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await expect(page).toHaveTitle('Weaver - Sessions');

    // The focused New Session route carries its own URL and title.
    await page.getByRole('button', { name: 'New session' }).click();
    await expect(page).toHaveTitle('Weaver - New Session');
    await expect(page).toHaveURL(/\/sessions\/new$/);

    await page.goto(`${weaver.baseUrl}/settings`);
    await expect(page).toHaveTitle('Weaver - Settings');

    await page.goto(`${weaver.baseUrl}/issues`);
    await expect(page).toHaveTitle('Weaver - Issues');

    await page.goto(`${weaver.baseUrl}/watches`);
    await expect(page).toHaveTitle('Weaver - Watches');
  });

  test('a session page titles the tab with the session name', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Title me', name: 'title-task' });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page).toHaveTitle('Weaver - title-task');
  });
});

test.describe('session rows are real links', () => {
  test('a row is an anchor to the detail page (openable in a new tab)', async ({ page, weaver }) => {
    const s = await weaver.seedSession({ goal: 'Open me in a tab', name: 'tab-task' });
    await page.goto(weaver.baseUrl);

    const card = page.locator(`[data-session-id="${s.id}"]`);
    // The stretched link makes the whole row a real <a href="/s/:id"> — the
    // element the browser's "Open link in new tab" / middle-click acts on.
    const link = card.locator(`a[href="/s/${s.id}"]`);
    await expect(link.first()).toBeVisible();

    // Left-click still navigates in-place.
    await card.click();
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}$`));
  });
});

test.describe('keyboard submit', () => {
  test('Ctrl/⌘+Enter in the form creates the session', async ({ page, weaver }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole('button', { name: 'New session' }).click();

    await page.getByPlaceholder('owner/name or /home/you/code/project').fill(weaver.repoPath);
    const goal = page.getByPlaceholder('Add a /health endpoint');
    await goal.fill('Built with the keyboard');

    // Submit from inside the goal textarea (a plain Enter there is a newline).
    await expect(page.getByTestId('create-session')).toBeEnabled();
    await goal.press('Control+Enter');

    await expect(page).toHaveURL(/\/s\/[^/]+$/);
    await page.locator('[data-rail="sessions"]').click();
    const card = page.getByTestId('session-card');
    await expect(card).toHaveCount(1);
    await expect(card.first()).toContainText('Built with the keyboard');
  });
});
