import type { Page } from "@playwright/test";
import { expect, test } from "../fixtures/weaver";

interface Group {
  id: string;
  name: string;
  session_ids: string[];
}

interface Layout {
  revision: number;
  spaces: { id: string; system_key: string | null; groups: Group[] }[];
}

async function getLayout(baseUrl: string): Promise<Layout> {
  return (await (
    await fetch(`${baseUrl}/api/session-layout`)
  ).json()) as Layout;
}

async function addGroup(baseUrl: string, name: string): Promise<Group> {
  const current = await getLayout(baseUrl);
  const user = current.spaces.find((space) => space.system_key === "user")!;
  const response = await fetch(`${baseUrl}/api/session-layout/groups`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      space_id: user.id,
      name,
      expected_revision: current.revision,
    }),
  });
  expect(response.ok).toBe(true);
  const next = (await response.json()) as Layout;
  return next.spaces
    .flatMap((space) => space.groups)
    .find((group) => group.name === name)!;
}

async function move(
  baseUrl: string,
  id: string,
  groupId: string,
  beforeId?: string,
): Promise<void> {
  const current = await getLayout(baseUrl);
  const response = await fetch(`${baseUrl}/api/session-layout/moves`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      session_ids: [id],
      destination_group_id: groupId,
      before_session_id: beforeId,
      expected_revision: current.revision,
    }),
  });
  expect(response.ok).toBe(true);
}

test.describe("durable placement interactions", () => {
  test("mouse drag moves to an empty group and persists after refresh", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Move with a pointer",
      name: "pointer-move",
    });
    const focused = await addGroup(weaver.baseUrl, "Focused");

    await page.goto(weaver.baseUrl);
    await expect(
      page.locator(
        `[data-session-id="${session.id}"] [data-testid="session-drag"]`,
      ),
    ).toBeVisible();
    await expect(page.locator(`[data-group-id="${focused.id}"]`)).toBeVisible();
    await pointerDragToGroup(page, session.id, focused.id);

    const target = page.locator(`[data-group-id="${focused.id}"]`);
    await expect(
      target.locator(`[data-session-id="${session.id}"]`),
    ).toBeVisible();
    await page.reload();
    await expect(
      target.locator(`[data-session-id="${session.id}"]`),
    ).toBeVisible();
  });

  test("keyboard Move has an immediate Undo path", async ({ page, weaver }) => {
    const session = await weaver.seedSession({
      goal: "Move with a keyboard",
      name: "keyboard-move",
    });
    const anchor = await weaver.seedSession({
      goal: "Exact keyboard insertion anchor",
      name: "keyboard-anchor",
    });
    const later = await addGroup(weaver.baseUrl, "Later");
    await move(weaver.baseUrl, anchor.id, later.id);

    await page.goto(weaver.baseUrl);
    const row = page.locator(`[data-session-id="${session.id}"]`);
    await row.getByTestId("move-session").focus();
    await row.getByTestId("move-session").press("Enter");
    await row.getByRole("combobox", { name: "Move to" }).selectOption(later.id);
    await row
      .getByRole("combobox", { name: "Position" })
      .selectOption(anchor.id);
    await row
      .getByTestId("move-session-panel")
      .getByRole("button", { name: "Move" })
      .press("Enter");

    const destination = page.locator(`[data-group-id="${later.id}"]`);
    await expect(
      destination.locator(`[data-session-id="${session.id}"]`),
    ).toBeVisible();
    await expect(
      destination.getByTestId("session-card").first(),
    ).toHaveAttribute("data-session-id", session.id);
    await expect(
      destination
        .locator(`[data-session-id="${session.id}"]`)
        .getByTestId("move-session"),
    ).toBeFocused();
    await page
      .getByTestId("move-undo")
      .getByRole("button", { name: "Undo" })
      .press("Enter");
    await expect(
      page
        .getByTestId("session-group")
        .filter({ hasText: "Inbox" })
        .locator(`[data-session-id="${session.id}"]`),
    ).toBeVisible();
  });

  test("moving into a collapsed group focuses Undo instead of a hidden row", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Collapsed target focus",
      name: "collapsed-target-move",
    });
    const collapsed = await addGroup(weaver.baseUrl, "Collapsed target");
    await page.goto(weaver.baseUrl);
    const target = page
      .getByTestId("session-group")
      .filter({ hasText: "Collapsed target" });
    await target
      .getByRole("button", { name: "Collapse Collapsed target" })
      .click();

    const row = page.locator(`[data-session-id="${session.id}"]`);
    await row.getByTestId("move-session").click();
    await row
      .getByRole("combobox", { name: "Move to" })
      .selectOption(collapsed.id);
    await row
      .getByTestId("move-session-panel")
      .getByRole("button", { name: "Move" })
      .click();

    await expect(
      target.locator(`[data-session-id="${session.id}"]`),
    ).toBeHidden();
    await expect(
      page.getByTestId("move-undo").getByRole("button", { name: "Undo" }),
    ).toBeFocused();
  });

  test("an external REST move updates live without losing selection or expansion", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Preserve local workbench state",
      name: "live-move",
    });
    const review = await addGroup(weaver.baseUrl, "Review");

    await page.goto(weaver.baseUrl);
    let row = page.locator(`[data-session-id="${session.id}"]`);
    await row.getByRole("checkbox", { name: "Select live-move" }).check();
    await row.getByTestId("session-details-toggle").click();
    await expect(row.getByTestId("session-preview")).toBeVisible();

    await move(weaver.baseUrl, session.id, review.id);

    row = page.locator(
      `[data-group-id="${review.id}"] [data-session-id="${session.id}"]`,
    );
    await expect(row).toBeVisible();
    await expect(
      row.getByRole("checkbox", { name: "Select live-move" }),
    ).toBeChecked();
    await expect(row.getByTestId("session-preview")).toBeVisible();
    await page.getByTestId("status-filter").selectOption("done");
    await expect(page.getByTestId("selection-toolbar")).toContainText(
      "1 hidden by this view",
    );
    await page.getByTestId("status-filter").selectOption("");
    await expect(row.getByTestId("session-preview")).toBeVisible();
    const reviewGroup = page
      .getByTestId("session-group")
      .filter({ hasText: "Review" });
    await reviewGroup.getByRole("button", { name: "Collapse Review" }).click();
    await expect(page.getByTestId("selection-toolbar")).toContainText(
      "1 hidden by this view",
    );
    await expect(row).toBeHidden();
    await reviewGroup.getByRole("button", { name: "Expand Review" }).click();
    await expect(row.getByTestId("session-preview")).toBeVisible();
  });

  test("the organizer exposes a cross-space group move", async ({
    page,
    weaver,
  }) => {
    await page.goto(weaver.baseUrl);
    await page.getByRole("button", { name: "Organize" }).click();
    await page.getByPlaceholder("New group").fill("Cross-space review");
    await page.getByRole("button", { name: "Add empty group" }).click();
    const organizer = page.getByTestId("layout-organizer");
    const destination = organizer.getByRole("combobox", {
      name: "Destination space for Cross-space review",
    });
    await destination.selectOption({ label: "GitHub" });
    await destination
      .locator("xpath=..")
      .getByRole("button", { name: "Move group" })
      .click();
    await organizer
      .getByRole("combobox", { name: "Space to organize" })
      .selectOption({ label: "GitHub" });
    await expect(
      organizer.getByLabel("Name for Cross-space review"),
    ).toBeVisible();
  });

  test("Undo remains retryable when the atomic restore fails", async ({
    page,
    weaver,
  }) => {
    const first = await weaver.seedSession({
      goal: "Atomic undo one",
      name: "undo-one",
    });
    const second = await weaver.seedSession({
      goal: "Atomic undo two",
      name: "undo-two",
    });
    const later = await addGroup(weaver.baseUrl, "Undo target");
    await page.goto(weaver.baseUrl);
    for (const [index, session] of [first, second].entries()) {
      const checkbox = page
        .locator(`[data-session-id="${session.id}"]`)
        .getByRole("checkbox");
      await checkbox.focus();
      await checkbox.press("Space");
      await expect(page.getByTestId("selection-toolbar")).toContainText(
        `${index + 1} selected`,
      );
    }
    await page
      .getByRole("combobox", { name: "Move selected to group" })
      .selectOption(later.id);
    await page
      .getByTestId("selection-toolbar")
      .getByRole("button", { name: "Move" })
      .click();

    await page.route("**/api/session-layout/restores", async (route) => {
      await route.fulfill({
        status: 409,
        contentType: "application/json",
        body: JSON.stringify({ error: "injected stale restore" }),
      });
    });
    await page
      .getByTestId("move-undo")
      .getByRole("button", { name: "Undo" })
      .click();
    await expect(page.getByTestId("move-undo")).toBeVisible();
    await expect(page.getByRole("alert")).toContainText(
      "workbench changed in another client",
    );
    for (const session of [first, second]) {
      await expect(
        page.locator(
          `[data-group-id="${later.id}"] [data-session-id="${session.id}"]`,
        ),
      ).toBeVisible();
    }

    await page.unroute("**/api/session-layout/restores");
    await page
      .getByTestId("move-undo")
      .getByRole("button", { name: "Undo" })
      .click();
    await expect(page.getByTestId("move-undo")).toHaveCount(0);
    await expect(page.getByRole("alert")).toHaveCount(0);
  });

  test("an open search recomputes membership for moves, renames, and deletion", async ({
    page,
    weaver,
  }) => {
    const placed = await weaver.seedSession({
      goal: "placement membership",
      name: "placed-search-result",
    });
    const entering = await weaver.seedSession({
      goal: "placement membership candidate",
      name: "entering-search-result",
    });
    const renamed = await weaver.seedSession({
      goal: "rename membership candidate",
      name: "rename-search-result",
    });
    const review = await addGroup(weaver.baseUrl, "Search review");
    await move(weaver.baseUrl, placed.id, review.id);
    await page.goto(weaver.baseUrl);
    const search = page.getByTestId("fleet-search");
    await search.fill("Search review");
    await expect(
      page.locator(`[data-session-id="${placed.id}"]`),
    ).toBeVisible();
    await expect(
      page.locator(`[data-session-id="${entering.id}"]`),
    ).toHaveCount(0);

    await move(weaver.baseUrl, placed.id, "group-user-inbox");
    await expect(page.locator(`[data-session-id="${placed.id}"]`)).toHaveCount(
      0,
    );
    await move(weaver.baseUrl, entering.id, review.id);
    await expect(
      page.locator(`[data-session-id="${entering.id}"]`),
    ).toBeVisible();
    const removed = await fetch(
      `${weaver.baseUrl}/api/sessions/${entering.id}`,
      { method: "DELETE" },
    );
    expect(removed.ok).toBe(true);
    await expect(
      page.locator(`[data-session-id="${entering.id}"]`),
    ).toHaveCount(0);

    await search.fill("renamed-into-query");
    await expect(page.locator(`[data-session-id="${renamed.id}"]`)).toHaveCount(
      0,
    );
    const renamedInto = await fetch(
      `${weaver.baseUrl}/api/sessions/${renamed.id}`,
      {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ title: "renamed-into-query" }),
      },
    );
    expect(renamedInto.ok).toBe(true);
    await expect(
      page.locator(`[data-session-id="${renamed.id}"]`),
    ).toBeVisible();
    const renamedOut = await fetch(
      `${weaver.baseUrl}/api/sessions/${renamed.id}`,
      {
        method: "PATCH",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ title: "outside again" }),
      },
    );
    expect(renamedOut.ok).toBe(true);
    await expect(page.locator(`[data-session-id="${renamed.id}"]`)).toHaveCount(
      0,
    );
    await expect(search).toHaveValue("renamed-into-query");
  });

  test("an invalidation during an older poll gets a trailing refresh", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Do not drop refreshes",
      name: "refresh-race",
    });
    const review = await addGroup(weaver.baseUrl, "Refresh destination");
    await page.goto(weaver.baseUrl);

    let releasePoll!: () => void;
    const pollReleased = new Promise<void>((resolve) => {
      releasePoll = resolve;
    });
    let pollCaptured!: () => void;
    const captured = new Promise<void>((resolve) => {
      pollCaptured = resolve;
    });
    let held = false;
    await page.route("**/api/sessions?archived=true", async (route) => {
      if (held) return route.continue();
      held = true;
      const oldResponse = await route.fetch();
      pollCaptured();
      await pollReleased;
      await route.fulfill({ response: oldResponse });
    });

    await addGroup(weaver.baseUrl, "Trigger old refresh");
    await captured;
    await move(weaver.baseUrl, session.id, review.id);
    releasePoll();

    await expect(
      page.locator(
        `[data-group-id="${review.id}"] [data-session-id="${session.id}"]`,
      ),
    ).toBeVisible();
    await page.unroute("**/api/sessions?archived=true");
  });
});

async function pointerDragToGroup(
  page: Page,
  sourceId: string,
  groupId: string,
) {
  const grip = page.locator(
    `[data-session-id="${sourceId}"] [data-testid="session-drag"]`,
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
