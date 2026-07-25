import { expect, test } from "../fixtures/weaver";

interface Layout {
  revision: number;
  spaces: {
    id: string;
    name: string;
    system_key: string | null;
    groups: { id: string; name: string; session_ids: string[] }[];
  }[];
}

async function seedAutomationSession(
  baseUrl: string,
  repoPath: string,
  opts: { name: string; goal: string },
) {
  const response = await fetch(`${baseUrl}/api/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      goal: opts.goal,
      title: opts.name,
      cwd: repoPath,
      agent: "shell",
      name: opts.name,
      class: "automation",
    }),
  });
  expect(response.ok).toBe(true);
  return (await response.json()) as { id: string };
}

async function moveToOps(baseUrl: string, sessionId: string) {
  const current = (await (
    await fetch(`${baseUrl}/api/session-layout`)
  ).json()) as Layout;
  const ops = current.spaces.find((space) => space.system_key === "ops")!;
  const response = await fetch(`${baseUrl}/api/session-layout/moves`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      session_ids: [sessionId],
      destination_group_id: ops.groups[0].id,
      expected_revision: current.revision,
    }),
  });
  expect(response.ok).toBe(true);
}

async function createFailedRun(baseUrl: string) {
  const response = await fetch(`${baseUrl}/api/runs`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      profile: "default",
      idempotency_key: `failed-launch-${Date.now()}`,
      source: "ops",
      service_tag: "fleet-maintenance",
      session: {
        cwd: "/definitely/missing/automation-repo",
        title: "will-not-launch",
        goal: "Exercise launch failure visibility",
        agent: "shell",
      },
    }),
  });
  expect(response.ok).toBe(false);
  await expect
    .poll(async () => {
      const runs = (await (await fetch(`${baseUrl}/api/runs`)).json()) as {
        status: string;
      }[];
      return runs[0]?.status;
    })
    .toBe("failed");
}

test.describe("automation unification", () => {
  test("successful automation sessions use the normal workbench and lifecycle controls", async ({
    page,
    weaver,
  }) => {
    const automation = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "watch-result",
        goal: "Audit the repository on a schedule",
      },
    );
    await moveToOps(weaver.baseUrl, automation.id);

    await page.goto(weaver.baseUrl);
    await page.getByRole("link", { name: /^Ops/ }).click();
    const row = page.locator(`[data-session-id="${automation.id}"]`);
    await expect(row).toBeVisible();
    await expect(row).toContainText("watch-result");
    await expect(row.getByTestId("move-session")).toBeVisible();
    await row.hover();
    await row.getByTestId("row-actions").click();
    await expect(row.getByTestId("row-action-auto-archive")).toBeVisible();

    await expect(page.getByTestId("automation-pane-link")).toHaveCount(0);
    await page.goto(`${weaver.baseUrl}/?view=automation`);
    await expect(page).not.toHaveTitle(/Automation/);
    await expect(
      page.getByRole("button", { name: "New session" }),
    ).toBeVisible();
  });

  test("automation sessions share Attention and History with user sessions", async ({
    page,
    weaver,
  }) => {
    const automation = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "needs-operator",
        goal: "Ask for a policy decision",
      },
    );
    const session = await weaver.getSession(automation.id);
    await weaver.setStatus(session, "blocked", "policy needs a decision");
    await moveToOps(weaver.baseUrl, automation.id);

    await page.goto(`${weaver.baseUrl}/?view=attention`);
    const row = page.locator(`[data-session-id="${automation.id}"]`);
    await expect(row).toBeVisible();
    await expect(row).toContainText("blocked");

    await weaver.archiveSession(automation.id);
    await page.getByTestId("history-view").click();
    await expect(row).toBeVisible();
    await expect(row.getByRole("link")).toContainText(
      "Ops / Inbox / needs-operator",
    );
  });

  test("a failed run archives into visible History while Remove deletes it", async ({
    page,
    weaver,
  }) => {
    await createFailedRun(weaver.baseUrl);

    await page.goto(`${weaver.baseUrl}/?view=attention`);
    const intervention = page.getByTestId("automation-run-only");
    await expect(page.getByTestId("interventions")).toBeVisible();
    await expect(intervention).toContainText("Launch failed");
    await expect(intervention).toContainText("ops");
    await expect(intervention.getByTestId("session-drag")).toHaveCount(0);
    await expect(page.getByTestId("status-bar-attention")).toContainText(
      "1 needs attention",
    );

    await intervention.hover();
    await intervention.getByTestId("run-actions").click();
    await intervention.getByTestId("run-action-archive").click();
    const dialog = page.getByTestId("confirm-dialog");
    await expect(dialog).toContainText("Archive launch attempt?");
    await dialog.getByTestId("confirm-dialog-confirm").click();
    await expect(intervention).toHaveCount(0);

    await page.getByTestId("history-view").click();
    const history = page.getByTestId("automation-run-history");
    const archived = history.getByTestId("automation-run-only");
    await expect(archived).toContainText("Run cancelled");
    await archived.hover();
    await archived.getByTestId("run-actions").click();
    await archived.getByTestId("run-action-remove").click();
    await expect(page.getByTestId("confirm-dialog")).toContainText(
      "Remove launch attempt?",
    );
    await page
      .getByTestId("confirm-dialog")
      .getByTestId("confirm-dialog-confirm")
      .click();
    await expect(archived).toHaveCount(0);
  });

  test("watch authoring remains available after the Automation pane is retired", async ({
    page,
    weaver,
  }) => {
    await page.goto(weaver.baseUrl);
    await expect(page.locator('[data-rail="watch"]')).toBeVisible();
    await page.locator('[data-rail="watch"]').click();
    await expect(page).toHaveURL(/\/watches$/);
    await expect(page.getByRole("heading", { name: /Watches/ })).toBeVisible();
  });
});
