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
): Promise<void> {
  const current = await getLayout(baseUrl);
  const response = await fetch(`${baseUrl}/api/session-layout/moves`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      session_ids: [id],
      destination_group_id: groupId,
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
    await synthDragToGroup(page, session.id, focused.id);

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
    const later = await addGroup(weaver.baseUrl, "Later");

    await page.goto(weaver.baseUrl);
    const row = page.locator(`[data-session-id="${session.id}"]`);
    await row.getByTestId("move-session").focus();
    await row.getByTestId("move-session").press("Enter");
    await row.getByRole("combobox", { name: "Move to" }).selectOption(later.id);
    await row
      .getByTestId("move-session-panel")
      .getByRole("button", { name: "Move" })
      .press("Enter");

    const destination = page.locator(`[data-group-id="${later.id}"]`);
    await expect(
      destination.locator(`[data-session-id="${session.id}"]`),
    ).toBeVisible();
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
  });
});

async function synthDragToGroup(page: Page, sourceId: string, groupId: string) {
  const transfer = await page.evaluateHandle(() => new DataTransfer());
  const grip = page.locator(
    `[data-session-id="${sourceId}"] [data-testid="session-drag"]`,
  );
  const target = page.locator(`[data-group-id="${groupId}"]`);
  await grip.dispatchEvent("dragstart", { dataTransfer: transfer });
  await target.dispatchEvent("dragover", { dataTransfer: transfer });
  await target.dispatchEvent("drop", { dataTransfer: transfer });
  await grip.dispatchEvent("dragend", { dataTransfer: transfer });
}
