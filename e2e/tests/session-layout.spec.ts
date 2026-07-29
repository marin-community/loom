import type { Page } from "@playwright/test";
import { expect, test } from "../fixtures/weaver";

interface Group {
  id: string;
  name: string;
  session_ids: string[];
}

interface Layout {
  revision: number;
  spaces: {
    id: string;
    system_key: string | null;
    groups: Group[];
  }[];
}

async function getLayout(baseUrl: string): Promise<Layout> {
  const response = await fetch(`${baseUrl}/api/session-layout`);
  expect(response.ok).toBe(true);
  return (await response.json()) as Layout;
}

async function move(
  baseUrl: string,
  sessionId: string,
  groupId: string,
  beforeSessionId?: string,
) {
  const current = await getLayout(baseUrl);
  const response = await fetch(`${baseUrl}/api/session-layout/moves`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      session_ids: [sessionId],
      destination_group_id: groupId,
      before_session_id: beforeSessionId,
      expected_revision: current.revision,
    }),
  });
  const body = await response.text();
  expect(response.ok, body).toBe(true);
}

async function pointerDragToGroup(
  page: Page,
  sessionId: string,
  groupId: string,
) {
  const grip = page.locator(
    `[data-session-id="${sessionId}"] [data-testid="session-drag"]`,
  );
  const target = page.locator(`[data-group-id="${groupId}"]`);
  const sourceBox = await grip.boundingBox();
  const targetBox = await target.boundingBox();
  expect(sourceBox).not.toBeNull();
  expect(targetBox).not.toBeNull();
  await page.mouse.move(
    sourceBox!.x + sourceBox!.width / 2,
    sourceBox!.y + sourceBox!.height / 2,
  );
  await page.mouse.down();
  await page.mouse.move(
    targetBox!.x + targetBox!.width / 2,
    targetBox!.y + targetBox!.height - 8,
    { steps: 16 },
  );
  await page.mouse.up();
}

async function seedAutomationSession(
  baseUrl: string,
  repoPath: string,
): Promise<{ id: string }> {
  const response = await fetch(`${baseUrl}/api/sessions`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      goal: "Automation fleet task",
      title: "automation-task",
      cwd: repoPath,
      agent: "shell",
      name: "automation-task",
      class: "automation",
    }),
  });
  expect(response.ok).toBe(true);
  return (await response.json()) as { id: string };
}

async function createFailedRun(baseUrl: string) {
  const response = await fetch(`${baseUrl}/api/runs`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      profile: "default",
      idempotency_key: `failed-workbench-${Date.now()}`,
      source: "ops",
      service_tag: "fleet-maintenance",
      session: {
        cwd: "/definitely/missing/automation-repo",
        title: "failed-automation-task",
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

test.describe("durable session workbench", () => {
  test("terminal mailbox commands navigate rows without stealing text input", async ({
    page,
    weaver,
  }) => {
    await weaver.seedSession({
      goal: "First keyboard-operated task",
      name: "keyboard-mailbox-one",
    });
    await weaver.seedSession({
      goal: "Second keyboard-operated task",
      name: "keyboard-mailbox-two",
    });
    await weaver.seedSession({
      goal: "Third keyboard-operated task",
      name: "keyboard-mailbox-three",
    });

    await page.goto(weaver.baseUrl);
    const rows = page.locator('[data-testid="session-card"]');
    await expect(rows).toHaveCount(3);
    await expect(page.locator('[data-ui="terminal"]')).toBeVisible();
    await expect(page.locator('[data-cursor="true"]')).toHaveCount(1);

    // Leave any browser-restored form focus before exercising application
    // commands; character shortcuts deliberately never steal input.
    await rows.nth(0).locator("[data-session-primary]").focus();
    await page.keyboard.press("Shift+/");
    const help = page.getByTestId("shortcut-help");
    await expect(help).toBeVisible();
    await expect(
      help.locator('[data-command-id="sessions.cursor-down"]'),
    ).toBeVisible();
    await expect(
      help.locator('[data-command-id="global.sessions"]'),
    ).toBeVisible();
    await page.keyboard.press("Tab");
    await expect(help.getByRole("button", { name: "Close" })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(help).toHaveCount(0);

    const firstId = await rows.nth(0).getAttribute("data-session-id");
    const secondId = await rows.nth(1).getAttribute("data-session-id");
    await page.keyboard.press("j");
    await expect(page.locator('[data-cursor="true"]')).toHaveAttribute(
      "data-session-id",
      secondId!,
    );
    await expect(
      page.locator(`[data-session-id="${secondId}"] [data-session-primary]`),
    ).toBeFocused();

    await page.keyboard.press("x");
    await expect(
      page.locator(`[data-session-id="${secondId}"]`).getByRole("checkbox"),
    ).toBeChecked();
    await page.keyboard.press("o");
    await expect(
      page
        .locator(`[data-session-id="${secondId}"]`)
        .getByTestId("session-preview"),
    ).toBeVisible();

    await page.keyboard.press("g");
    await expect(page.getByTestId("command-chord")).toContainText("g");
    await page.keyboard.press("g");
    await expect(page.locator('[data-cursor="true"]')).toHaveAttribute(
      "data-session-id",
      firstId!,
    );

    await page.keyboard.press("/");
    const search = page.getByTestId("fleet-search");
    await expect(search).toBeFocused();
    await page.keyboard.type("j");
    await expect(search).toHaveValue("j");

    await search.fill("");
    await page.getByTestId("status-bar").click();
    await page.keyboard.press("g");
    await page.keyboard.press("i");
    await expect(page).toHaveURL(`${weaver.baseUrl}/issues`);
    await page.keyboard.press("g");
    await page.keyboard.press("s");
    await expect(page).toHaveURL(weaver.baseUrl + "/");
  });

  test("fleet polling stays compact and row details load on demand", async ({
    page,
    weaver,
  }) => {
    const goal = "Full operator context fetched only after disclosure";
    const session = await weaver.seedSession({
      goal,
      name: "compact-fleet-row",
    });
    const summaryResponse = page.waitForResponse((response) => {
      const url = new URL(response.url());
      return (
        response.request().method() === "GET" &&
        url.pathname === "/api/sessions/summary" &&
        !url.searchParams.has("archived") &&
        url.searchParams.get("automation") === "true"
      );
    });
    const detailRequests: string[] = [];
    page.on("request", (request) => {
      const url = new URL(request.url());
      if (
        request.method() === "GET" &&
        url.pathname === `/api/sessions/${session.id}`
      ) {
        detailRequests.push(url.pathname);
      }
    });

    await page.goto(weaver.baseUrl);
    const summary = (await (await summaryResponse).json()) as Array<
      Record<string, unknown> & { id: string; branch: Record<string, unknown> }
    >;
    const rowSummary = summary.find(
      (candidate) => candidate.id === session.id,
    )!;
    expect(rowSummary.branch.goal).toBeUndefined();
    expect(rowSummary.resolved_launch).toBeUndefined();

    const row = page.locator(`[data-session-id="${session.id}"]`);
    await expect(row).toBeVisible();
    expect(detailRequests).toEqual([]);

    await row.getByTestId("session-details-toggle").click();
    await expect
      .poll(() => detailRequests)
      .toEqual([`/api/sessions/${session.id}`]);
    await expect(row.getByTestId("session-preview")).toContainText(goal);
  });

  test("@workbench pointer, keyboard, undo, preference, and SSE share one layout", async ({
    page,
    weaver,
  }) => {
    const pointer = await weaver.seedSession({
      goal: "Pointer placement",
      name: "pointer-task",
    });
    const keyboard = await weaver.seedSession({
      goal: "Keyboard placement",
      name: "keyboard-task",
    });

    await page.goto(weaver.baseUrl);
    await page.getByRole("button", { name: "Organize" }).click();
    await page.getByPlaceholder("New group").fill("Journey Focus");
    await page.getByRole("button", { name: "Add empty group" }).click();
    const target = page
      .getByTestId("session-group")
      .filter({ hasText: "Journey Focus" });
    await expect(target.getByTestId("empty-group")).toBeVisible();
    const group = (await getLayout(weaver.baseUrl)).spaces
      .flatMap((space) => space.groups)
      .find((candidate) => candidate.name === "Journey Focus")!;

    await pointerDragToGroup(page, pointer.id, group.id);
    await expect(
      target.locator(`[data-session-id="${pointer.id}"]`),
    ).toBeVisible();

    let keyboardRow = page.locator(`[data-session-id="${keyboard.id}"]`);
    await keyboardRow.getByTestId("session-details-toggle").click();
    await keyboardRow.getByTestId("move-session").click();
    await keyboardRow
      .getByRole("combobox", { name: "Move to" })
      .selectOption(group.id);
    await keyboardRow
      .getByRole("combobox", { name: "Position" })
      .selectOption(pointer.id);
    await keyboardRow
      .getByTestId("move-session-panel")
      .getByRole("button", { name: "Move" })
      .click();
    await expect(target.getByTestId("session-card").first()).toHaveAttribute(
      "data-session-id",
      keyboard.id,
    );
    await page
      .getByTestId("move-undo")
      .getByRole("button", { name: "Undo" })
      .click();
    await expect(
      page
        .locator('[data-group-id="group-user-inbox"]')
        .locator(`[data-session-id="${keyboard.id}"]`),
    ).toBeVisible();
    await expect(
      target.locator(`[data-session-id="${pointer.id}"]`),
    ).toBeVisible();

    await target
      .getByRole("button", { name: "Collapse Journey Focus" })
      .click();
    await page.reload();
    await expect(
      target.getByRole("button", { name: "Expand Journey Focus" }),
    ).toBeVisible();
    await target.getByRole("button", { name: "Expand Journey Focus" }).click();

    keyboardRow = page.locator(`[data-session-id="${keyboard.id}"]`);
    await keyboardRow
      .getByRole("checkbox", { name: "Select keyboard-task" })
      .check();
    await keyboardRow.getByTestId("session-details-toggle").click();
    await expect(keyboardRow.getByTestId("session-preview")).toBeVisible();
    await move(weaver.baseUrl, keyboard.id, group.id, pointer.id);

    keyboardRow = target.locator(`[data-session-id="${keyboard.id}"]`);
    await expect(keyboardRow).toBeVisible();
    await expect(
      keyboardRow.getByRole("checkbox", { name: "Select keyboard-task" }),
    ).toBeChecked();
    await expect(keyboardRow.getByTestId("session-preview")).toBeVisible();
    await target
      .getByRole("button", { name: "Collapse Journey Focus" })
      .click();
    await expect(page.getByTestId("selection-toolbar")).toContainText(
      "1 hidden by this view",
    );
  });

  test("@workbench fleet search, Attention, History, recovery, and interventions", async ({
    page,
    weaver,
  }) => {
    const normal = await weaver.seedSession({
      goal: "Normal searchable fleet task",
      name: "normal-task",
    });
    const automation = await seedAutomationSession(
      weaver.baseUrl,
      weaver.repoPath,
    );
    const automationView = await weaver.getSession(automation.id);
    await weaver.setStatus(
      automationView,
      "blocked",
      "automation needs an operator",
    );
    const ops = (await getLayout(weaver.baseUrl)).spaces.find(
      (space) => space.system_key === "ops",
    )!;
    await move(weaver.baseUrl, automation.id, ops.groups[0].id);
    await createFailedRun(weaver.baseUrl);

    await page.goto(`${weaver.baseUrl}/?view=attention`);
    const automationRow = page.locator(`[data-session-id="${automation.id}"]`);
    await expect(automationRow).toBeVisible();
    await expect(page.getByTestId("automation-run-only")).toContainText(
      "Launch failed",
    );
    await expect(page.getByTestId("status-bar-attention")).toContainText(
      "need",
    );

    await page.getByTestId("all-view").click();
    const search = page.getByTestId("fleet-search");
    await search.fill("Normal searchable");
    const normalRow = page.locator(`[data-session-id="${normal.id}"]`);
    await expect(normalRow).toBeVisible();
    await expect(normalRow.getByRole("link")).toContainText(
      "Inbox / normal-task",
    );
    await expect(normalRow.getByRole("link")).not.toContainText("User /");
    await search.fill("");
    await page.getByTestId("attention-filter").selectOption("blocked");
    await expect(automationRow).toBeVisible();
    await expect(normalRow).toHaveCount(0);
    await page.getByTestId("attention-filter").selectOption("");

    for (const session of [normal, automation]) {
      await page
        .locator(`[data-session-id="${session.id}"]`)
        .getByRole("checkbox")
        .check();
    }
    await page
      .getByTestId("selection-toolbar")
      .getByRole("button", { name: "Archive" })
      .click();
    await expect(page.getByTestId("confirm-dialog")).toContainText(
      "2 selected sessions",
    );
    await page
      .getByTestId("confirm-dialog")
      .getByTestId("confirm-dialog-confirm")
      .click();

    // Archived summaries are not part of the recurring snapshot, but widened
    // server search can still render a cold result before History is opened.
    await search.fill("Normal searchable");
    await expect(normalRow).toHaveCount(0);
    await page.getByTestId("search-history").check();
    await expect(normalRow).toBeVisible();
    await search.fill("");

    await page.getByTestId("history-view").click();
    const archivedNormal = page.locator(`[data-session-id="${normal.id}"]`);
    const archivedAutomation = page.locator(
      `[data-session-id="${automation.id}"]`,
    );
    await expect(archivedNormal.getByRole("link")).toContainText(
      "Inbox / normal-task",
    );
    await expect(archivedAutomation.getByRole("link")).toContainText(
      "Inbox / automation-task",
    );
    await archivedNormal.getByTestId("remedy-recover").click();
    await expect(archivedNormal).toHaveCount(0);
    await expect
      .poll(async () => (await weaver.getSession(normal.id)).status)
      .not.toBe("archived");

    await page.getByTestId("attention-view").click();
    const intervention = page.getByTestId("automation-run-only");
    await intervention.hover();
    await intervention.getByTestId("run-actions").click();
    await intervention.getByTestId("run-action-archive").click();
    await page
      .getByTestId("confirm-dialog")
      .getByTestId("confirm-dialog-confirm")
      .click();
    await page.getByTestId("history-view").click();
    const cancelled = page
      .getByTestId("automation-run-history")
      .getByTestId("automation-run-only");
    await expect(cancelled).toContainText("Run cancelled");
    await cancelled.hover();
    await cancelled.getByTestId("run-actions").click();
    await cancelled.getByTestId("run-action-remove").click();
    await page
      .getByTestId("confirm-dialog")
      .getByTestId("confirm-dialog-confirm")
      .click();
    await expect(cancelled).toHaveCount(0);
  });
});
