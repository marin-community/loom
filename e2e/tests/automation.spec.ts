import { test, expect } from "../fixtures/weaver";

// Phase 1: automation session visibility. Sessions launched by a background
// surface (a webhook, a watch, `/marinbot`, …) are stamped `class:
// 'automation'` at create time and hidden from the fleet list by default — the
// list is a person's workbench, not a log of every agent turn the server ever
// ran. The Automation toggle (alongside the archived one) reveals them as
// ordinary cards, with a quiet origin pill naming the surface that launched
// them. See docs/loom-ui.md and crates/loom/frontend/src/views/SessionList.vue.
//
// `origin` is server-stamped and cannot be supplied by an API caller: a plain
// create lands as `user`, and one carrying `parent_branch` as `agent` — the
// only non-`user` origin reachable from a test, so that is what the pill
// coverage uses.

/** Create a session directly via the API with an explicit `class` —
 *  `weaver.seedSession` always creates an ordinary interactive session, so an
 *  automation-class row is seeded with a raw POST mirroring its request body. */
async function seedAutomationSession(
  baseUrl: string,
  repoPath: string,
  opts: { name: string; goal: string; parentBranch?: string },
) {
  const res = await fetch(`${baseUrl}/api/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      goal: opts.goal,
      title: opts.name,
      cwd: repoPath,
      agent: "shell",
      name: opts.name,
      class: "automation",
      parent_branch: opts.parentBranch,
    }),
  });
  if (!res.ok) {
    throw new Error(
      `seed automation session failed: ${res.status} ${await res.text()}`,
    );
  }
  return res.json();
}

test.describe("automation session visibility", () => {
  test("an automation-class session is hidden from the default session list", async ({
    page,
    weaver,
  }) => {
    await seedAutomationSession(weaver.baseUrl, weaver.repoPath, {
      name: "watch-triggered",
      goal: "Triggered by a watch round",
    });

    await page.goto(weaver.baseUrl);

    // Hidden by default — the fleet reads as empty, not "1 session".
    await expect(page.getByText("No sessions yet.")).toBeVisible();
    await expect(page.getByTestId("session-card")).toHaveCount(0);
  });

  test("the Automation toggle reveals an automation-class session", async ({
    page,
    weaver,
  }) => {
    const parent = await weaver.seedSession({
      goal: "Parent work",
      name: "parent-task",
    });
    // A sub-session launch (`parent_branch`) is stamped `origin: 'agent'` —
    // the pill under test.
    const automation = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "delegated-run",
        goal: "Delegated by the parent session",
        parentBranch: parent.branch.id,
      },
    );

    await page.goto(weaver.baseUrl);
    await expect(page.getByTestId("session-card")).toHaveCount(1);

    const toggle = page.getByTestId("automation-toggle");
    await expect(toggle).toContainText("Show 1 automation");
    await toggle.click();

    const card = page.locator(`[data-session-id="${automation.id}"]`);
    await expect(card).toBeVisible();
    await expect(card).toContainText("delegated-run");
    // A non-`user` origin renders as a quiet identity pill on the card.
    await expect(card.getByTestId("origin-pill")).toHaveText("agent");

    await expect(toggle).toContainText("Hide 1 automation");
    await toggle.click();
    await expect(page.getByTestId("session-card")).toHaveCount(1);
  });

  test("an ordinary session still appears by default alongside a hidden automation one", async ({
    page,
    weaver,
  }) => {
    const ordinary = await weaver.seedSession({
      goal: "Regular work",
      name: "ordinary-task",
    });
    await seedAutomationSession(weaver.baseUrl, weaver.repoPath, {
      name: "ops-triggered",
      goal: "Launched by an ops script",
    });

    await page.goto(weaver.baseUrl);

    await expect(page.getByTestId("session-card")).toHaveCount(1);
    const card = page.locator(`[data-session-id="${ordinary.id}"]`);
    await expect(card).toBeVisible();
    await expect(card).toContainText("ordinary-task");
    // A `user`-origin session (the default) carries no origin pill.
    await expect(card.getByTestId("origin-pill")).toHaveCount(0);

    await page.getByTestId("automation-toggle").click();
    await expect(page.getByTestId("session-card")).toHaveCount(2);
  });
});
