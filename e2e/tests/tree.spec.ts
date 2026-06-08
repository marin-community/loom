import { test, expect } from '../fixtures/weaver';

// The session list threads sub-sessions under the session that launched them:
// a child hangs below its parent (resolved from the tracking issue's
// source_branch → branch.parent_id), siblings sort newest-first, and a subtle
// gutter draws the ├/└ connectors. Top-level sessions draw no gutter, so a flat
// fleet is unchanged.
test.describe('session tree', () => {
  test('nests children under the session that launched them', async ({ page, weaver }) => {
    // parent ─┬─ child-b      (newest sibling first)
    //         └─ child-a ──── grandchild
    const parent = await weaver.seedSession({ goal: 'Build the feature', name: 'parent' });
    const childA = await weaver.seedSession({
      goal: 'Backend work',
      name: 'child-a',
      parent: parent.branch.id,
    });
    const childB = await weaver.seedSession({
      goal: 'Frontend work',
      name: 'child-b',
      parent: parent.branch.id,
    });
    const grandchild = await weaver.seedSession({
      goal: 'CSS guides',
      name: 'grandchild',
      parent: childA.branch.id,
    });

    await page.goto(weaver.baseUrl);
    const cards = page.getByTestId('session-card');
    await expect(cards).toHaveCount(4);

    const card = (id: string) => page.locator(`[data-session-id="${id}"]`);

    // Depth: parent at the root, its children one in, the grandchild two in.
    await expect(card(parent.id)).toHaveAttribute('data-depth', '0');
    await expect(card(childA.id)).toHaveAttribute('data-depth', '1');
    await expect(card(childB.id)).toHaveAttribute('data-depth', '1');
    await expect(card(grandchild.id)).toHaveAttribute('data-depth', '2');

    // Top-level rows draw no gutter; nested rows do (one column per depth level).
    await expect(card(parent.id).locator('.tree-gutter')).toHaveCount(0);
    await expect(card(childA.id).locator('.tree-gutter')).toHaveCount(1);
    await expect(card(grandchild.id).locator('.tree-col')).toHaveCount(2);

    // DFS order: the parent leads, and the grandchild renders immediately under
    // its own parent (child-a) — never detached from its thread.
    const ids = await cards.evaluateAll((els) =>
      els.map((e) => e.getAttribute('data-session-id')),
    );
    expect(ids[0]).toBe(parent.id);
    expect(ids.indexOf(grandchild.id)).toBe(ids.indexOf(childA.id) + 1);
    // Newest sibling first: child-b (seeded last of the two) precedes child-a.
    expect(ids.indexOf(childB.id)).toBeLessThan(ids.indexOf(childA.id));
  });

  test('a parent archived out of view drops its child to the top level', async ({
    page,
    weaver,
  }) => {
    const parent = await weaver.seedSession({ goal: 'Parent task', name: 'parent' });
    const child = await weaver.seedSession({
      goal: 'Child task',
      name: 'child',
      parent: parent.branch.id,
    });

    // Archive the parent — by default archived rows are hidden, so the child has
    // no visible parent and falls back to a top-level (gutter-less) row.
    await fetch(`${weaver.baseUrl}/api/sessions/${parent.id}/archive`, { method: 'POST' });

    await page.goto(weaver.baseUrl);
    const childCard = page.locator(`[data-session-id="${child.id}"]`);
    await expect(childCard).toBeVisible();
    await expect(childCard).toHaveAttribute('data-depth', '0');
    await expect(childCard.locator('.tree-gutter')).toHaveCount(0);
  });
});
