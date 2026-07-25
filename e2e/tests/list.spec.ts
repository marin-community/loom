import { expect, test } from "../fixtures/weaver";

interface LayoutGroup {
  id: string;
  name: string;
  session_ids: string[];
}

interface LayoutSpace {
  id: string;
  name: string;
  system_key: string | null;
  groups: LayoutGroup[];
}

interface Layout {
  revision: number;
  spaces: LayoutSpace[];
}

async function layout(baseUrl: string): Promise<Layout> {
  return (await (
    await fetch(`${baseUrl}/api/session-layout`)
  ).json()) as Layout;
}

async function createGroup(
  baseUrl: string,
  spaceId: string,
  name: string,
): Promise<Layout> {
  const current = await layout(baseUrl);
  const response = await fetch(`${baseUrl}/api/session-layout/groups`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      space_id: spaceId,
      name,
      expected_revision: current.revision,
    }),
  });
  expect(response.ok).toBe(true);
  return (await response.json()) as Layout;
}

async function move(
  baseUrl: string,
  sessionIds: string[],
  groupId: string,
): Promise<Layout> {
  const current = await layout(baseUrl);
  const response = await fetch(`${baseUrl}/api/session-layout/moves`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      session_ids: sessionIds,
      destination_group_id: groupId,
      expected_revision: current.revision,
    }),
  });
  expect(response.ok).toBe(true);
  return (await response.json()) as Layout;
}

test.describe("durable session workbench", () => {
  test("keeps empty groups useful and session details quiet until requested", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Add a health endpoint without crowding the fleet",
      name: "health-endpoint",
    });

    await page.goto(weaver.baseUrl);
    const row = page.locator(`[data-session-id="${session.id}"]`);
    await expect(row).toBeVisible();
    await expect(row).toContainText("health-endpoint");

    const preview = row.getByTestId("session-preview");
    await expect(preview).toBeHidden();
    await row.hover();
    await expect(preview).toBeVisible();
    await expect(preview).toContainText("Add a health endpoint");

    await row.getByTestId("session-details-toggle").click();
    await page.mouse.move(0, 0);
    await expect(preview).toBeVisible();

    await page.getByRole("button", { name: "Organize" }).click();
    await page.getByPlaceholder("New group").fill("Waiting");
    await page.getByRole("button", { name: "Add empty group" }).click();
    const empty = page
      .getByTestId("session-group")
      .filter({ hasText: "Waiting" })
      .getByTestId("empty-group");
    await expect(empty).toContainText("drop sessions here or use Move");

    await page.getByTestId("status-filter").selectOption("done");
    const filteredInbox = page
      .getByTestId("session-group")
      .filter({ hasText: "Inbox" })
      .getByTestId("empty-group");
    await expect(filteredInbox).toContainText("No sessions match");
    await expect(filteredInbox).not.toContainText("Empty group");

    await page.reload();
    await expect(
      page.getByTestId("session-group").filter({ hasText: "Waiting" }),
    ).toBeVisible();
  });

  test("searches prompts and qualified placement names, with optional History", async ({
    page,
    weaver,
  }) => {
    const current = await weaver.seedSession({
      goal: "Investigate a very specific websocket regression",
      name: "socket-current",
    });
    const archived = await weaver.seedSession({
      goal: "Document an old migration result",
      name: "migration-history",
    });
    await weaver.archiveSession(archived.id);

    const initial = await layout(weaver.baseUrl);
    const user = initial.spaces.find((space) => space.system_key === "user")!;
    const withGroup = await createGroup(weaver.baseUrl, user.id, "Deep Focus");
    const focused = withGroup.spaces
      .flatMap((space) => space.groups)
      .find((group) => group.name === "Deep Focus")!;
    await move(weaver.baseUrl, [current.id], focused.id);

    await page.goto(weaver.baseUrl);
    const search = page.getByTestId("fleet-search");
    await search.fill("websocket regression");
    const currentRow = page.locator(`[data-session-id="${current.id}"]`);
    await expect(currentRow).toBeVisible();
    await expect(currentRow.getByRole("link")).toContainText(
      "User / Deep Focus / socket-current",
    );

    await search.fill("migration result");
    await expect(
      page.locator(`[data-session-id="${archived.id}"]`),
    ).toHaveCount(0);
    await page.getByTestId("search-history").check();
    const historyRow = page.locator(`[data-session-id="${archived.id}"]`);
    await expect(historyRow).toBeVisible();
    await expect(historyRow.getByRole("link")).toContainText(
      "User / Inbox / migration-history",
    );
  });

  test("status and attention filters are URL-backed and compose", async ({
    page,
    weaver,
  }) => {
    const calm = await weaver.seedSession({ goal: "Calm work", name: "calm" });
    const blocked = await weaver.seedSession({
      goal: "Blocked work",
      name: "blocked",
    });
    await weaver.setStatus(blocked, "blocked", "needs an operator decision");

    await page.goto(weaver.baseUrl);
    await page.getByTestId("attention-filter").selectOption("blocked");
    await expect(page).toHaveURL(/attention=blocked/);
    await expect(
      page.locator(`[data-session-id="${blocked.id}"]`),
    ).toBeVisible();
    await expect(page.locator(`[data-session-id="${calm.id}"]`)).toHaveCount(0);

    await page.getByTestId("status-filter").selectOption("running");
    await expect(page).toHaveURL(/status=running/);
    await expect(
      page.locator(`[data-session-id="${blocked.id}"]`),
    ).toBeVisible();

    await page.getByTestId("attention-filter").selectOption("");
    await expect(page.locator(`[data-session-id="${calm.id}"]`)).toBeVisible();
    await page.getByTestId("attention-view").click();
    await expect(
      page.locator(`[data-session-id="${blocked.id}"]`),
    ).toBeVisible();
    await expect(page.locator(`[data-session-id="${calm.id}"]`)).toHaveCount(0);
  });

  test("mechanical failures are urgent and carry a textual lifecycle at rest", async ({
    page,
    weaver,
  }) => {
    const rows: { id: string; status: string }[] = [];
    for (const status of ["created", "done", "error", "orphaned"]) {
      const session = await weaver.seedSession({
        goal: `${status} without relying on color`,
        name: `lifecycle-${status}`,
      });
      const response = await fetch(
        `${weaver.baseUrl}/api/sessions/${session.id}`,
        {
          method: "PATCH",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ status }),
        },
      );
      expect(response.ok).toBe(true);
      rows.push({ id: session.id, status });
    }

    await page.goto(weaver.baseUrl);
    for (const row of rows) {
      await expect(
        page
          .locator(`[data-session-id="${row.id}"]`)
          .getByLabel(`Lifecycle: ${row.status}`),
      ).toBeVisible();
    }
    await expect(page.getByTestId("status-bar-attention")).toContainText(
      "2 need attention",
    );
    await page.getByTestId("attention-view").click();
    await expect(page.locator('[aria-label="Lifecycle: error"]')).toBeVisible();
    await expect(
      page.locator('[aria-label="Lifecycle: orphaned"]'),
    ).toBeVisible();
    await expect(page.locator('[aria-label="Lifecycle: created"]')).toHaveCount(
      0,
    );
    await expect(page.locator('[aria-label="Lifecycle: done"]')).toHaveCount(0);
  });

  test("multi-selection Archive confirms scope and reports aggregate success", async ({
    page,
    weaver,
  }) => {
    const sessions = await Promise.all([
      weaver.seedSession({ goal: "Archive one", name: "archive-one" }),
      weaver.seedSession({ goal: "Archive two", name: "archive-two" }),
    ]);
    await page.goto(weaver.baseUrl);
    for (const [index, session] of sessions.entries()) {
      const checkbox = page
        .locator(`[data-session-id="${session.id}"]`)
        .getByRole("checkbox");
      await checkbox.focus();
      await checkbox.press("Space");
      await expect(page.getByTestId("selection-toolbar")).toContainText(
        `${index + 1} selected`,
      );
    }
    await page.route(
      `**/api/sessions/${sessions[1].id}/archive`,
      async (route) => {
        await route.fulfill({
          status: 503,
          contentType: "application/json",
          body: JSON.stringify({ error: "injected archive failure" }),
        });
      },
    );
    await page
      .getByTestId("selection-toolbar")
      .getByRole("button", { name: "Archive" })
      .click();
    const dialog = page.getByTestId("confirm-dialog");
    await expect(dialog).toContainText("2 selected sessions");
    await dialog.getByTestId("confirm-dialog-confirm").click();
    await expect(page.getByTestId("archive-result")).toContainText(
      "Archived 1 session; 1 failed and remain selected",
    );
    await expect(page.getByTestId("selection-toolbar")).toContainText(
      "1 selected",
    );
    await page.unroute(`**/api/sessions/${sessions[1].id}/archive`);
    await page
      .getByTestId("selection-toolbar")
      .getByRole("button", { name: "Archive" })
      .click();
    await page
      .getByTestId("confirm-dialog")
      .getByTestId("confirm-dialog-confirm")
      .click();
    await page.getByTestId("history-view").click();
    for (const session of sessions) {
      await expect(
        page.locator(`[data-session-id="${session.id}"]`),
      ).toBeVisible();
    }
  });

  test("History preserves location and recovered sessions return to that group", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Recover into the same durable location",
      name: "recover-location",
    });
    const initial = await layout(weaver.baseUrl);
    const user = initial.spaces.find((space) => space.system_key === "user")!;
    const withGroup = await createGroup(weaver.baseUrl, user.id, "Review");
    const review = withGroup.spaces
      .flatMap((space) => space.groups)
      .find((group) => group.name === "Review")!;
    await move(weaver.baseUrl, [session.id], review.id);
    await weaver.archiveSession(session.id);

    await page.goto(`${weaver.baseUrl}/?history=true`);
    const row = page.locator(`[data-session-id="${session.id}"]`);
    await expect(row).toBeVisible();
    await expect(row.getByRole("link")).toContainText(
      "User / Review / recover-location",
    );
    await row.getByTestId("remedy-recover").click();

    await expect(row).toHaveCount(0);
    await page.getByRole("link", { name: /^User/ }).click();
    const recovered = page.locator(`[data-session-id="${session.id}"]`);
    await expect(recovered).toBeVisible();
    const after = await layout(weaver.baseUrl);
    expect(
      after.spaces
        .flatMap((space) => space.groups)
        .find((group) => group.id === review.id)?.session_ids,
    ).toContain(session.id);
  });

  test("group collapse is an operator preference that survives reload", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Stay grouped",
      name: "collapsed-row",
    });
    await page.goto(weaver.baseUrl);

    const inbox = page
      .getByTestId("session-group")
      .filter({ hasText: "Inbox" });
    const toggle = inbox.getByRole("button", { name: "Collapse Inbox" });
    await toggle.click();
    await expect(
      inbox.locator(`[data-session-id="${session.id}"]`),
    ).toBeHidden();

    await page.reload();
    await expect(
      inbox.getByRole("button", { name: "Expand Inbox" }),
    ).toBeVisible();
    await expect(
      inbox.locator(`[data-session-id="${session.id}"]`),
    ).toBeHidden();
  });
});
