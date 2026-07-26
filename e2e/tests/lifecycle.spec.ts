import { test, expect } from "../fixtures/weaver";

// The lifecycle actions (Adopt / Recover / Archive / Remove) are reachable from
// two places: the detail header's Details popover, and each fleet-list row's ⋯
// menu. A stuck session (orphaned/archived) also carries its remedy as a plain
// button next to the status badge, on both surfaces.
test.describe("session lifecycle actions", () => {
  test("Remove (confirmed) deletes the session and returns to the list", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Delete me",
      name: "remove-task",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect(
      page.getByRole("heading", { name: "remove-task" }),
    ).toBeVisible();

    await page.getByRole("button", { name: /Details/ }).click();

    // Remove uses a native confirm() dialog — accept it.
    page.once("dialog", (dialog) => {
      expect(dialog.type()).toBe("confirm");
      dialog.accept();
    });
    await page.getByRole("button", { name: "Remove" }).click();

    // Router pushes back to the list.
    await expect(page).toHaveURL(/\/\?space=space-user$/);
    await expect(page.getByRole("heading", { name: "Sessions" })).toBeVisible();
    await expect(page.getByTestId("session-card")).toHaveCount(0);
    await expect(page.getByTestId("empty-group")).toBeVisible();

    // And it is gone server-side.
    const all = await weaver.listSessions();
    expect(all).toHaveLength(0);
  });

  test("dismissing the confirm dialog keeps the session", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({ goal: "Keep me", name: "keep-task" });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole("button", { name: /Details/ }).click();

    page.once("dialog", (dialog) => dialog.dismiss());
    await page.getByRole("button", { name: "Remove" }).click();

    // Still on the detail page, still present server-side.
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}$`));
    const all = await weaver.listSessions();
    expect(all).toHaveLength(1);
  });

  test("Archive (confirmed) tears down the session but keeps its record", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Archive me",
      name: "archive-task",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole("button", { name: /Details/ }).click();

    page.once("dialog", (dialog) => {
      expect(dialog.type()).toBe("confirm");
      dialog.accept();
    });
    await page.getByTestId("action-archive").click();

    // The header reloads into the archived state: the lifecycle badge appears
    // and the popover's Archive button goes away (archiving twice is a no-op).
    await expect(page.getByTestId("status-badge")).toHaveText(/archived/i);
    await expect(page.getByTestId("action-archive")).toHaveCount(0);

    // Server-side the session row survives — archived, not deleted.
    const updated = await weaver.getSession(s.id);
    expect(updated.status).toBe("archived");
  });

  test("lifecycle actions stay on-screen in a short window", async ({
    page,
    weaver,
  }) => {
    // Regression: the details popover used to grow past the bottom of the page
    // in a short window, clipping expanded context and Archive/Remove out of
    // reach. The whole variable-height body now shares a bounded scroller.
    const s = await weaver.seedSession({
      goal: Array.from(
        { length: 40 },
        (_, index) => `Goal context line ${index + 1}`,
      ).join("\n"),
      name: "short-window-task",
    });

    await page.setViewportSize({ width: 1280, height: 300 });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole("button", { name: /Details/ }).click();
    await page.getByText("Goal / prompt", { exact: true }).click();

    const scroller = page.getByTestId("details-scroll");
    await expect(scroller).toHaveCSS("overflow-y", "auto");
    const bounds = await scroller.evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
    expect(bounds.scrollHeight).toBeGreaterThan(bounds.clientHeight);

    await page.getByTestId("session-goal-context").scrollIntoViewIfNeeded();
    await expect(page.getByTestId("session-goal-context")).toBeVisible();
    for (const [name, id] of [
      ["Archive", "action-archive"],
      ["Remove", "action-remove"],
    ]) {
      const action = page.getByTestId(id);
      await action.scrollIntoViewIfNeeded();
      const box = await action.boundingBox();
      expect(box, `${name} button should render`).not.toBeNull();
      expect(box!.y).toBeGreaterThanOrEqual(0);
      expect(box!.y + box!.height).toBeLessThanOrEqual(300);
    }

    // And Remove genuinely works from here.
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Remove" }).click();
    await expect(page).toHaveURL(/\/\?space=space-user$/);
    expect(await weaver.listSessions()).toHaveLength(0);
  });

  test("a session can opt out of automatic archive from Details", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Keep me live",
      name: "no-auto-archive",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole("button", { name: /Details/ }).click();
    await page.getByTestId("action-auto-archive").click();

    await expect(page.getByTestId("tag-pill")).toContainText(
      "auto-archive: disabled",
    );
    await expect
      .poll(async () => (await weaver.getSession(s.id)).branch.tags)
      .toContainEqual(
        expect.objectContaining({ key: "auto-archive", value: "disabled" }),
      );

    await expect(page.getByTestId("action-auto-archive")).toContainText(
      "Enable auto-archive",
    );
    await page.getByTestId("action-auto-archive").click();

    await expect(page.getByTestId("tag-pill")).toHaveCount(0);
    await expect
      .poll(async () => (await weaver.getSession(s.id)).branch.tags)
      .not.toContainEqual(expect.objectContaining({ key: "auto-archive" }));
  });
});
