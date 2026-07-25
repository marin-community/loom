import { expect, test } from "../fixtures/weaver";

interface Layout {
  revision: number;
  spaces: {
    id: string;
    system_key: string | null;
    groups: { id: string; name: string; session_ids: string[] }[];
  }[];
}

async function getLayout(baseUrl: string): Promise<Layout> {
  return (await (
    await fetch(`${baseUrl}/api/session-layout`)
  ).json()) as Layout;
}

test.describe("session lineage in the flat workbench", () => {
  test("lineage metadata remains visible without recreating a recursive tree", async ({
    page,
    weaver,
  }) => {
    const parent = await weaver.seedSession({
      goal: "Coordinate work",
      name: "parent",
    });
    const initial = await getLayout(weaver.baseUrl);
    const user = initial.spaces.find((space) => space.system_key === "user")!;
    const create = await fetch(`${weaver.baseUrl}/api/session-layout/groups`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        space_id: user.id,
        name: "Delegated",
        expected_revision: initial.revision,
      }),
    });
    const withGroup = (await create.json()) as Layout;
    const delegated = withGroup.spaces
      .flatMap((space) => space.groups)
      .find((group) => group.name === "Delegated")!;
    await fetch(`${weaver.baseUrl}/api/session-layout/moves`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        session_ids: [parent.id],
        destination_group_id: delegated.id,
        expected_revision: withGroup.revision,
      }),
    });

    const child = await weaver.seedSession({
      goal: "Handle the delegated part",
      name: "child",
      parent: parent.branch.id,
    });
    const beforeChildMove = await getLayout(weaver.baseUrl);
    const moveChild = await fetch(
      `${weaver.baseUrl}/api/session-layout/moves`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          session_ids: [child.id],
          destination_group_id: delegated.id,
          expected_revision: beforeChildMove.revision,
        }),
      },
    );
    expect(moveChild.ok).toBe(true);

    await page.goto(weaver.baseUrl);
    const section = page.locator(`[data-group-id="${delegated.id}"]`);
    await expect(
      section.locator(`[data-session-id="${parent.id}"]`),
    ).toBeVisible();
    const childRow = section.locator(`[data-session-id="${child.id}"]`);
    await expect(childRow).toBeVisible();
    await expect(childRow).not.toHaveAttribute("data-depth");
    await childRow.getByTestId("session-details-toggle").click();
    await expect(childRow.getByTestId("session-preview")).toContainText(
      `delegated from ${parent.branch.id}`,
    );
  });
});
