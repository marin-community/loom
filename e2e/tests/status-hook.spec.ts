import { test, expect } from "../fixtures/weaver";

test.describe("status reflects hook and attention events", () => {
  test('detail view: hooks drive lifecycle and a waiting lull reads as a calm "Idle"', async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Watch my status",
      name: "hook-detail",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    // The agent's derived state shows on the quiet conversation-state strip.
    const conv = page.getByTestId("conversation-state");
    await expect(conv).toBeVisible();

    // A `working` hook means a prompt was submitted — the agent process is alive
    // → lifecycle `running` (the silent default: no lifecycle badge), and an
    // engaged agent reads as "Working".
    await weaver.hook(s, "working");
    await expect(page.getByTestId("status-badge")).toHaveCount(0);
    await expect(conv).toContainText("Working");

    // A `waiting` hook (a Notification lull) is no longer loud: it stamps the
    // soothing idle mark rather than raising attention (see monitor.rs::apply_hook).
    // The mark lands on the branch…
    await weaver.hook(s, "waiting");
    await expect
      .poll(async () =>
        (await weaver.getSession(s.id)).branch.tags.some(
          (t) => t.key === "idle",
        ),
      )
      .toBe(true);
    // …but the session stays calm — the loud attention axis is never raised.
    await expect(
      page.locator('[data-testid="signal-chip"][data-signal-key="attention"]'),
    ).toHaveCount(0);
    // …and the detail strip resolves to the calm "Idle" — the restful state, not
    // stuck on "Working". #247 regression guard: the idle mark must outlive the
    // monitor's pane-hash touch that bumps `last_activity_at` just after the
    // mark's `set_at`, or `idleTag()` reads the mark as stale and the strip never
    // leaves "Working".
    await expect(conv).toContainText("Idle");
  });

  test("detail view: weaver status sets level + message via SSE", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Declare my status",
      name: "status-detail",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const conv = page.getByTestId("conversation-state");
    await expect(conv).toBeVisible();

    await weaver.setStatus(s, "blocked", "tests failing, need help");
    await expect(
      page.locator('[data-testid="signal-chip"][data-signal-key="attention"]'),
    ).toHaveAttribute("data-level", "blocked");
    await expect(page.getByTestId("status-message")).toHaveText(
      /tests failing, need help/i,
    );
  });
});
