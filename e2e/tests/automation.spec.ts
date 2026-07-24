import { test, expect } from "../fixtures/weaver";

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

async function setLifecycle(baseUrl: string, id: string, status: string) {
  const res = await fetch(`${baseUrl}/api/sessions/${id}`, {
    method: "PATCH",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ status }),
  });
  if (!res.ok)
    throw new Error(`set lifecycle failed: ${res.status} ${await res.text()}`);
}

test.describe("automation session surface", () => {
  test("Workspace is the default and Automation is an isolated, linkable pane", async ({
    page,
    weaver,
  }) => {
    const ordinary = await weaver.seedSession({
      goal: "Regular work",
      name: "ordinary-task",
    });
    const automation = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "comment-rewrite",
        goal: "Rewrite a GitHub comment",
        parentBranch: ordinary.branch.id,
      },
    );

    await page.goto(weaver.baseUrl);

    await expect(page.getByTestId("workspace-pane-link")).toHaveAttribute(
      "aria-current",
      "page",
    );
    await expect(page.getByTestId("session-card")).toHaveCount(1);
    await expect(
      page.locator(`[data-session-id="${ordinary.id}"]`),
    ).toBeVisible();
    await expect(
      page.locator(`[data-session-id="${automation.id}"]`),
    ).toHaveCount(0);

    const automationLink = page.getByTestId("automation-pane-link");
    await expect(automationLink).toHaveAttribute("href", /view=automation/);
    await automationLink.click();

    await expect(page).toHaveURL(/view=automation/);
    await expect(automationLink).toHaveAttribute("aria-current", "page");
    await expect(page.getByTestId("session-card")).toHaveCount(0);
    const row = page.locator(`[data-session-id="${automation.id}"]`);
    await expect(row).toBeVisible();
    await expect(row).toContainText("comment-rewrite");
    await expect(row).toContainText("agent");
    await expect(row).toContainText("launched by ordinary-task");
    await expect(page.getByRole("button", { name: "New session" })).toHaveCount(
      0,
    );

    await page.goBack();
    await expect(page.getByTestId("workspace-pane-link")).toHaveAttribute(
      "aria-current",
      "page",
    );
    await expect(
      page.locator(`[data-session-id="${ordinary.id}"]`),
    ).toBeVisible();
    await page.goForward();
    await expect(page.getByTestId("automation-pane")).toBeVisible();

    await row.click();
    await expect(page).toHaveURL(new RegExp(`/s/${automation.id}$`));
    await page.getByRole("link", { name: "← automation" }).click();
    await expect(page).toHaveURL(/view=automation/);
  });

  test("direct Automation loads ignore Workspace-only query controls", async ({
    page,
    weaver,
  }) => {
    await seedAutomationSession(weaver.baseUrl, weaver.repoPath, {
      name: "direct-run",
      goal: "Open the operations pane directly",
    });

    await page.goto(`${weaver.baseUrl}/?view=automation&new&filter=attention`);

    await expect(page.getByTestId("automation-pane")).toBeVisible();
    await expect(page.getByTestId("filter-attention")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "New session" })).toHaveCount(
      0,
    );
    await expect(page.getByText("Create session")).toHaveCount(0);
    await expect(page).toHaveTitle("Weaver - Automation");

    await page.goto(`${weaver.baseUrl}/?view=unknown&history=true`);
    await expect(page.getByTestId("workspace-pane-link")).toHaveAttribute(
      "aria-current",
      "page",
    );
    await expect(page.getByTestId("automation-pane")).toHaveCount(0);
  });

  test("exceptions are ordered blocked, lifecycle failures, then attention", async ({
    page,
    weaver,
  }) => {
    const blocked = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "blocked-run",
        goal: "Blocked",
      },
    );
    const errored = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "error-run",
        goal: "Errored",
      },
    );
    const orphaned = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "orphaned-run",
        goal: "Orphaned",
      },
    );
    const attention = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "attention-run",
        goal: "Attention",
      },
    );
    const calm = await seedAutomationSession(weaver.baseUrl, weaver.repoPath, {
      name: "calm-run",
      goal: "Calm",
    });

    await weaver.setStatus(blocked, "blocked", "policy needs a decision");
    await setLifecycle(weaver.baseUrl, errored.id, "error");
    await setLifecycle(weaver.baseUrl, orphaned.id, "orphaned");
    await weaver.setStatus(attention, "attention", "review the generated text");

    await page.goto(weaver.baseUrl);
    await expect(
      page.getByTestId("automation-intervention-badge"),
    ).toContainText("4 need intervention");
    await page.getByTestId("automation-pane-link").click();

    const ids = await page
      .getByTestId("automation-interventions")
      .getByTestId("automation-session")
      .evaluateAll((rows) =>
        rows.map((row) => row.getAttribute("data-session-id")),
      );
    expect(ids[0]).toBe(blocked.id);
    expect(new Set(ids.slice(1, 3))).toEqual(
      new Set([errored.id, orphaned.id]),
    );
    expect(ids[3]).toBe(attention.id);

    await expect(
      page
        .getByTestId("automation-active")
        .locator(`[data-session-id="${calm.id}"]`),
    ).toBeVisible();
  });

  test("done and archived sessions live only in URL-backed History", async ({
    page,
    weaver,
  }) => {
    const done = await seedAutomationSession(weaver.baseUrl, weaver.repoPath, {
      name: "done-run",
      goal: "Done",
    });
    const archived = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "archived-run",
        goal: "Archived",
      },
    );
    await setLifecycle(weaver.baseUrl, done.id, "done");
    await weaver.archiveSession(archived.id);

    await page.goto(`${weaver.baseUrl}/?view=automation`);
    await expect(
      page.getByTestId("automation-active").getByTestId("automation-session"),
    ).toHaveCount(0);
    await expect(page.getByTestId("automation-history")).toBeHidden();

    const historyToggle = page.getByTestId("automation-history-toggle");
    await expect(historyToggle).toHaveAttribute("aria-expanded", "false");
    await historyToggle.click();
    await expect(page).toHaveURL(
      /view=automation.*history=true|history=true.*view=automation/,
    );
    await expect(historyToggle).toHaveAttribute("aria-expanded", "true");
    await expect(page.getByTestId("automation-history")).toBeVisible();
    await expect(
      page
        .getByTestId("automation-history")
        .locator(`[data-session-id="${done.id}"]`),
    ).toBeVisible();
    await expect(
      page
        .getByTestId("automation-history")
        .locator(`[data-session-id="${archived.id}"]`),
    ).toBeVisible();

    await page.reload();
    await expect(page.getByTestId("automation-history")).toBeVisible();
  });

  test("automation sessions expose the same lifecycle controls as workspace sessions", async ({
    page,
    weaver,
  }) => {
    const session = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
      {
        name: "administer-run",
        goal: "Administer this automation",
      },
    );

    await page.goto(`${weaver.baseUrl}/?view=automation`);
    const active = page
      .getByTestId("automation-active")
      .locator(`[data-session-id="${session.id}"]`);
    await active.hover();
    await active.getByTestId("row-actions").click();
    page.once("dialog", (dialog) => dialog.accept());
    await active.getByTestId("row-action-archive").click();
    await expect(active).toHaveCount(0);

    await page.getByTestId("automation-history-toggle").click();
    await expect(
      page
        .getByTestId("automation-history")
        .locator(`[data-session-id="${session.id}"]`),
    ).toBeVisible();
  });

  test("a failed launch with no session remains visible", async ({
    page,
    weaver,
  }) => {
    const response = await fetch(`${weaver.baseUrl}/api/runs`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        profile: "default",
        idempotency_key: `failed-launch-${Date.now()}`,
        source: "ops",
        session: {
          cwd: "/definitely/missing/automation-repo",
          title: "will-not-launch",
          goal: "Exercise launch failure visibility",
          agent: "shell",
        },
      }),
    });
    expect(response.ok).toBe(false);
    const failure = await response.text();
    expect(failure).toContain("must be automation-class");
    await expect
      .poll(async () => {
        const runs = await fetch(`${weaver.baseUrl}/api/runs`);
        return (await runs.json()) as Array<{ status: string }>;
      })
      .toEqual([expect.objectContaining({ status: "failed" })]);

    await page.goto(weaver.baseUrl);
    await expect(
      page.getByTestId("automation-intervention-badge"),
    ).toContainText("1 need intervention");
    await page.getByTestId("automation-pane-link").click();

    const failed = page.getByTestId("automation-run-only");
    await expect(failed).toContainText("Launch failed");
    await expect(failed).toContainText("ops");
    await expect(failed).toContainText("default");
  });

  test("an unmatched running reservation remains active", async ({
    page,
    weaver,
  }) => {
    const now = new Date().toISOString();
    await page.route("**/api/runs", async (route) => {
      await route.fulfill({
        json: [
          {
            id: "running-reservation",
            actor_subject: "automation:test",
            source: "actions",
            service_tag: "comment-rewriter",
            profile: "default",
            idempotency_key: "running-reservation",
            channel: null,
            session_id: "not-yet-visible",
            status: "running",
            outcome: null,
            summary: "",
            created_at: now,
            updated_at: now,
          },
          {
            id: "cancelled-reservation",
            actor_subject: "automation:test",
            source: "actions",
            service_tag: "comment-rewriter",
            profile: "default",
            idempotency_key: "cancelled-reservation",
            channel: null,
            session_id: "removed-session",
            status: "cancelled",
            outcome: "cancelled",
            summary: "session removed by user",
            created_at: now,
            updated_at: now,
          },
        ],
      });
    });

    await page.goto(`${weaver.baseUrl}/?view=automation`);

    await expect(page.getByTestId("automation-intervention-badge")).toHaveCount(
      0,
    );
    await expect(
      page.getByTestId("automation-active").getByTestId("automation-run-only"),
    ).toContainText("running");
    await expect(page.getByTestId("automation-interventions")).toHaveCount(0);
    await page.getByTestId("automation-history-toggle").click();
    await expect(
      page
        .getByTestId("automation-history")
        .getByTestId("automation-run-only"),
    ).toContainText("Run cancelled");
  });
});
