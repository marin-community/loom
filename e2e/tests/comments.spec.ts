import { test, expect } from "../fixtures/weaver";
import type { Page } from "@playwright/test";

const DOC = [
  "# Design notes",
  "",
  "We keep the markdown representation as the default, and layer",
  "collaborative editing on top of it.",
  "",
  "## Open questions",
  "",
  "- Should comments wait for one explicit review submission?",
  "- How do anchors survive an edit elsewhere in the document?",
  "",
].join("\n");

/** Select rendered text. Dispatching `selectionchange` models keyboard-created
 * selection; the review affordance must not depend on a mouseup. */
async function selectPhrase(page: Page, phrase: string, mouseup = true) {
  await page.evaluate(
    ({ needle, mouse }) => {
      const body = document.querySelector(".markdown-body") as HTMLElement;
      const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT);
      let node: Text | null = null;
      let index = -1;
      for (
        let current = walker.nextNode();
        current;
        current = walker.nextNode()
      ) {
        const at = (current as Text).data.indexOf(needle);
        if (at !== -1) {
          node = current as Text;
          index = at;
          break;
        }
      }
      if (!node)
        throw new Error(`phrase not found in rendered body: ${needle}`);
      const range = document.createRange();
      range.setStart(node, index);
      range.setEnd(node, index + needle.length);
      const selection = window.getSelection()!;
      selection.removeAllRanges();
      selection.addRange(range);
      document.dispatchEvent(new Event("selectionchange"));
      if (mouse)
        body.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
    },
    { needle: phrase, mouse: mouseup },
  );
}

async function addPendingComment(
  page: Page,
  phrase: string,
  body: string,
  mouseup = true,
) {
  await expect(page.locator(".markdown-body")).toContainText(phrase);
  await selectPhrase(page, phrase, mouseup);
  const affordance = page.getByTestId("review-selection-button");
  await expect(affordance).toBeVisible();
  await affordance.click();
  const composer = page.getByTestId("review-comment-composer");
  await composer.locator("textarea").fill(body);
  await composer.getByRole("button", { name: "Add pending comment" }).click();
  await expect(composer).toBeHidden();
}

test.describe("goal as an artifact", () => {
  test("a seeded goal remains a first-class rendered artifact", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "# Ship the search rewrite\n\nMake it **fast** and incremental.",
      name: "goal-artifact",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/goal`);
    await expect(page.locator('[data-artifact="goal"]')).toContainText(
      "branch",
    );
    await expect(page.locator(".markdown-body h1")).toContainText(
      "Ship the search rewrite",
    );
    await expect(page.locator(".markdown-body strong")).toContainText("fast");
  });
});

test.describe("staged artifact reviews", () => {
  test("creates and cancels a comment through the keyboard selection path", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "keyboard review",
      name: "review-keyboard",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await expect(page.locator(".markdown-body h1")).toContainText(
      "Design notes",
    );

    const article = page.locator(".markdown-body");
    await article.focus();
    for (let index = 0; index < 6; index += 1) {
      await page.keyboard.press("Shift+ArrowRight");
    }
    const affordance = page.getByTestId("review-selection-button");
    await expect(affordance).toBeVisible();
    await page.keyboard.press("Tab");
    await expect(affordance).toBeFocused();
    await page.keyboard.press("Enter");
    const composer = page.getByTestId("review-comment-composer");
    await expect(composer.locator("textarea")).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(composer).toBeHidden();
    await expect(page.getByTestId("review-tray-toggle")).toBeFocused();

    await article.focus();
    for (let index = 0; index < 6; index += 1) {
      await page.keyboard.press("Shift+ArrowRight");
    }
    await page.keyboard.press("Tab");
    await page.keyboard.press("Enter");
    await composer.locator("textarea").fill("Keyboard-only pending feedback.");
    await page.keyboard.press("Tab");
    await expect(
      composer.getByRole("button", { name: "Add pending comment" }),
    ).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(composer).toBeHidden();
    await expect(page.getByTestId("review-tray")).toContainText("1 pending");
  });

  test("drafts multiple comments, edits, deletes, and survives reload", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "review drafting",
      name: "review-draft",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await expect(page.locator(".markdown-body h1")).toContainText(
      "Design notes",
    );

    // The first selection is keyboard-shaped: no mouseup dependency.
    await addPendingComment(
      page,
      "collaborative editing",
      "Clarify whether editing is source-only.",
      false,
    );
    await addPendingComment(
      page,
      "anchors survive",
      "Call out the captured context used during drift.",
    );

    const tray = page.getByTestId("review-tray");
    await expect(tray).toContainText("2 pending");
    const cards = page.locator('[data-testid^="review-comment-"]');
    await expect(cards).toHaveCount(2);

    const second = cards.nth(1);
    await expect(second).toContainText("captured context");
    await second.focus();
    await page.keyboard.press("Enter");
    await expect(second).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(second.getByRole("button", { name: "Edit" })).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(second.getByTestId("review-comment-edit")).toBeFocused();
    await second
      .getByTestId("review-comment-edit")
      .fill("Explain prefix and suffix recovery.");
    await page.keyboard.press("Escape");
    await expect(second.getByRole("button", { name: "Edit" })).toBeFocused();
    await page.keyboard.press("Enter");
    await second
      .getByTestId("review-comment-edit")
      .fill("Explain prefix and suffix recovery.");
    await second.getByRole("button", { name: "Save" }).click();
    await expect(second.getByRole("button", { name: "Edit" })).toBeFocused();
    await expect(second).toContainText("Explain prefix and suffix recovery.");
    await page.keyboard.press("Escape");
    await expect(second).toHaveAttribute("data-review-collapsed");
    await expect(second).toBeFocused();

    // Durable server-side draft state comes back after a full document reload.
    await page.reload();
    await expect(page.getByTestId("review-tray")).toContainText("2 pending");
    await expect(page.locator('[data-testid^="review-comment-"]')).toHaveCount(
      2,
    );
    const edited = page
      .locator('[data-testid^="review-comment-"]')
      .filter({ hasText: "Explain prefix and suffix recovery." });
    await edited.click();
    await expect(edited).toContainText("Explain prefix and suffix recovery.");

    await edited.getByRole("button", { name: "Delete" }).click();
    await expect(page.getByTestId("review-tray")).toContainText("1 pending");
    await expect(page.locator('[data-testid^="review-comment-"]')).toHaveCount(
      1,
    );
  });

  test("preserves stale anchors, re-anchors, previews, and submits once", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "stale review",
      name: "review-stale",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);

    await addPendingComment(
      page,
      "collaborative editing",
      "This statement needs an explicit boundary.",
    );

    const revised = DOC.replace(
      "collaborative editing on top of it.",
      "staged review feedback on top of it.",
    );
    await weaver.writeArtifact(session, "design", revised, {
      title: "Design notes",
    });
    await expect(page.locator(".markdown-body")).toContainText(
      "staged review feedback",
    );

    const tray = page.getByTestId("review-tray");
    await expect(tray).toContainText("stale");
    await expect(page.getByTestId("review-stale-warning")).toBeVisible();
    await expect(page.getByTestId("review-stale-anchors")).toBeVisible();

    const staleCard = page.locator('[data-testid^="review-comment-"]').first();
    await tray
      .getByRole("button", {
        name: /This statement needs an explicit boundary/,
      })
      .click();
    await expect(staleCard).toBeFocused();
    await expect(staleCard).toBeInViewport();
    await staleCard.getByRole("button", { name: "Re-anchor" }).click();
    await selectPhrase(page, "staged review feedback");
    await page.getByTestId("review-selection-button").click();
    await expect(staleCard).toContainText("staged review feedback");
    await expect(staleCard).not.toContainText("stale");
    await expect(page.getByTestId("review-stale-warning")).toBeHidden();

    await page
      .getByTestId("review-overall-note")
      .fill("Address this before landing.");
    await expect(tray).toContainText("Conversation feedback preview");
    await expect(tray).toContainText("Address this before landing.");
    await expect(tray).toContainText(
      "This statement needs an explicit boundary.",
    );

    // Re-anchoring the final old comment truthfully advances the envelope, so
    // the exact preview can submit without a stale acknowledgement.
    const submitRequest = page.waitForRequest(
      (request) =>
        request.method() === "POST" &&
        /\/api\/reviews\/\d+\/submit$/.test(request.url()),
    );
    await page.getByTestId("submit-review").click();
    await submitRequest;
    await expect(tray).toContainText("Review submitted");
    await expect(tray).toContainText(/queued|delivered/);
    await expect(page.getByTestId("submit-review")).toHaveCount(0);
  });

  test("cancels orphan re-anchor mode and keeps resolution errors on the card", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "review recovery",
      name: "review-recovery",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await addPendingComment(
      page,
      "collaborative editing",
      "Keep this recovery path visible.",
    );
    await weaver.writeArtifact(
      session,
      "design",
      DOC.replace("collaborative editing", "staged replacement"),
      { title: "Design notes" },
    );
    await expect(page.getByTestId("review-stale-anchors")).toBeVisible();

    const card = page
      .locator('[data-testid^="review-comment-"]')
      .filter({ hasText: "Keep this recovery path visible." });
    if (await card.getByRole("button", { name: "Re-anchor" }).count()) {
      await card.getByRole("button", { name: "Re-anchor" }).click();
    } else {
      await card.click();
      await card.getByRole("button", { name: "Re-anchor" }).click();
    }
    await selectPhrase(page, "staged replacement");
    const selection = page.getByTestId("review-selection-button");
    await selection.focus();
    await page.keyboard.press("Escape");
    await expect(selection).toBeHidden();
    await expect(card.getByRole("button", { name: "Edit" })).toBeFocused();

    await card.getByRole("button", { name: "Re-anchor" }).click();
    await selectPhrase(page, "staged replacement");
    await page.getByTestId("review-selection-button").click();
    await page.getByTestId("submit-review").click();
    await expect(page.getByTestId("review-tray")).toContainText(
      "Review submitted",
    );

    await card.click();
    await page.route(
      /\/api\/reviews\/\d+\/comments\/\d+\/resolve$/,
      async (route) => {
        await route.fulfill({
          status: 500,
          contentType: "application/json",
          body: JSON.stringify({ error: "forced resolution failure" }),
        });
      },
      { times: 1 },
    );
    await card.getByRole("button", { name: "Resolve" }).click();
    await expect(card.getByRole("alert")).toContainText(
      "forced resolution failure",
    );
  });

  test("persists an overall-only draft through reload and pop, then discards a new draft", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "overall review",
      name: "review-overall",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await page.getByTestId("review-tray-toggle").click();
    const note = page.getByTestId("review-overall-note");
    await page.route(
      `**/api/sessions/${session.id}/reviews`,
      async (route) => {
        await new Promise((resolve) => setTimeout(resolve, 200));
        await route.continue();
      },
      { times: 1 },
    );
    await note.fill("Overall feedback survives every layout.");
    const create = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        response.url().endsWith(`/api/sessions/${session.id}/reviews`),
    );
    await page.getByTestId("artifact-pop").click();
    await create;
    await expect(page.getByTestId("artifact-pop")).toContainText("Dock");
    await page.getByTestId("review-tray-toggle").click();
    await expect(page.getByTestId("review-tray-toggle")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    await expect(page.getByTestId("review-overall-note")).toHaveValue(
      "Overall feedback survives every layout.",
    );

    const summarySave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await page
      .getByTestId("review-overall-note")
      .fill("Overall feedback survives every layout and debounce.");
    await page.getByTestId("artifact-pop").click();
    await summarySave;
    await expect(page.getByTestId("artifact-pop")).toContainText("Pop out");
    await page.getByTestId("review-tray-toggle").click();
    await expect(page.getByTestId("review-tray-toggle")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    await expect(page.getByTestId("review-overall-note")).toHaveValue(
      "Overall feedback survives every layout and debounce.",
    );

    await weaver.writeArtifact(session, "design", `${DOC}\nRevision two.\n`, {
      title: "Design notes",
    });
    await expect(page.getByTestId("review-stale-warning")).toBeVisible();
    await page.getByTestId("review-retarget-current").click();
    await expect(page.getByTestId("review-stale-warning")).toBeHidden();

    await page.reload();
    await page.getByTestId("review-tray-toggle").click();
    await expect(page.getByTestId("review-overall-note")).toHaveValue(
      "Overall feedback survives every layout and debounce.",
    );
    await page.getByTestId("submit-review").click();
    await expect(page.getByTestId("review-tray")).toContainText(
      "Review submitted",
    );

    const secondSave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await page
      .getByTestId("review-overall-note")
      .fill("This second overall-only draft will be discarded.");
    await page.getByTestId("review-overall-note").blur();
    await secondSave;
    await expect(page.getByTestId("review-tray")).toContainText(
      "Discard draft",
    );
    await page.getByRole("button", { name: "Discard draft" }).click();
    await page.getByRole("button", { name: "Discard", exact: true }).click();
    await expect(page.getByTestId("review-tray")).not.toContainText(
      "This second overall-only draft will be discarded.",
    );
  });

  test("projects optimistic conflicts and rejects a stale multi-tab submit", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "multi-tab review",
      name: "review-multi-tab",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await page.getByTestId("review-tray-toggle").click();
    const firstSave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await page.getByTestId("review-overall-note").fill("Initial shared draft.");
    await firstSave;

    const stalePage = await page.context().newPage();
    let frozenList: {
      status: number;
      headers: Record<string, string>;
      body: string;
    } | null = null;
    await stalePage.route(
      `**/api/sessions/${session.id}/reviews?subject_kind=artifact&subject_key=design`,
      async (route) => {
        if (frozenList) {
          await route.fulfill(frozenList);
          return;
        }
        const response = await route.fetch();
        frozenList = {
          status: response.status(),
          headers: response.headers(),
          body: await response.text(),
        };
        await route.fulfill(frozenList);
      },
    );
    await stalePage.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await stalePage.getByTestId("review-tray-toggle").click();
    await expect(stalePage.getByTestId("review-overall-note")).toHaveValue(
      "Initial shared draft.",
    );

    const ownerSave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await page.getByTestId("review-overall-note").fill("Newer owner draft.");
    await ownerSave;
    await stalePage
      .getByTestId("review-overall-note")
      .fill("Conflicting stale edit.");
    await stalePage.getByTestId("review-overall-note").blur();
    await expect(stalePage.getByRole("alert")).toContainText(
      "draft changed elsewhere",
    );
    await expect(stalePage.getByTestId("review-overall-note")).toHaveValue(
      "Newer owner draft.",
    );

    const finalSave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await page
      .getByTestId("review-overall-note")
      .fill("Frozen submitted truth.");
    await finalSave;
    await page.getByTestId("submit-review").click();
    await expect(page.getByTestId("review-tray")).toContainText(
      "Review submitted",
    );

    const staleSubmit = stalePage.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        /\/api\/reviews\/\d+\/submit$/.test(response.url()),
    );
    await stalePage.getByTestId("submit-review").click();
    expect((await staleSubmit).status()).toBe(409);
    await expect(stalePage.getByRole("alert")).toContainText(
      "draft changed elsewhere",
    );
    await expect(
      stalePage.locator('[data-testid="submit-review"]:visible'),
    ).toHaveCount(0);
    await stalePage.close();
  });

  test("projects legacy comments and clears resolved highlights", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "legacy projection",
      name: "review-legacy",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    const response = await page.request.post(
      `${weaver.baseUrl}/api/sessions/${session.id}/artifacts/design/threads`,
      {
        data: {
          base_rev: 1,
          anchor: {
            quote: "collaborative editing",
            prefix: "",
            suffix: "",
          },
          body: "Earlier submitted feedback remains actionable.",
        },
      },
    );
    expect(response.ok()).toBe(true);
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);

    const legacy = page
      .locator('[data-testid^="review-comment-"]')
      .filter({ hasText: "Earlier submitted feedback" });
    await expect(legacy).toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            "highlights" in CSS &&
            (CSS.highlights as Map<string, unknown>).has("weaver-comment"),
        ),
      )
      .toBe(true);
    await legacy.click();
    await legacy.getByRole("button", { name: "Resolve" }).click();
    await expect(legacy).toContainText("Resolved");
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            "highlights" in CSS &&
            (CSS.highlights as Map<string, unknown>).has("weaver-comment"),
        ),
      )
      .toBe(false);
  });

  test("reviews historical revisions in warm tabs and both themes", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "warm historical review",
      name: "review-warm-history",
    });
    await weaver.writeArtifact(
      session,
      "design",
      `${DOC}\nRevision one marker.\n`,
      {
        title: "Design notes",
      },
    );
    await weaver.writeArtifact(
      session,
      "design",
      `${DOC}\nRevision two marker.\n`,
      {
        title: "Design notes",
      },
    );
    await page.goto(`${weaver.baseUrl}/s/${session.id}`);
    await page.locator('[data-tab="artifacts"]').click();
    await page.locator('[data-artifact="design"]').click();
    const revisions = page.getByTestId("artifact-rev");
    await revisions.selectOption("1");
    await expect(page.locator(".markdown-body")).toContainText(
      "Revision one marker",
    );
    await addPendingComment(
      page,
      "collaborative editing",
      "Historical revision feedback.",
    );
    await expect(page.getByTestId("review-tray")).toContainText("stale");

    await page.locator('[data-tab="terminal"]').click();
    await page.locator('[data-tab="artifacts"]').click();
    await expect(page.getByTestId("review-tray")).toContainText("1 pending");

    for (const theme of ["light", "dark"]) {
      await page.evaluate((nextTheme) => {
        localStorage.setItem("loom-theme", nextTheme);
        document.documentElement.classList.toggle("dark", nextTheme === "dark");
      }, theme);
      await expect
        .poll(() =>
          page
            .locator("html")
            .evaluate((root) => root.classList.contains("dark")),
        )
        .toBe(theme === "dark");
      await expect(
        page.locator('[data-testid^="review-comment-"]').first(),
      ).toBeVisible();
    }
  });

  test("keeps the captured revision while a pending selection waits", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "revision-safe review",
      name: "review-captured-revision",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    await expect(page.locator(".markdown-body h1")).toContainText(
      "Design notes",
    );

    await selectPhrase(page, "collaborative editing");
    await page.getByTestId("review-selection-button").click();
    const composer = page.getByTestId("review-comment-composer");
    await composer
      .locator("textarea")
      .fill("Keep this tied to the text I selected.");

    await weaver.writeArtifact(
      session,
      "design",
      DOC.replace("default, and layer", "default, then layer"),
      { title: "Design notes" },
    );
    await expect(page.locator(".markdown-body")).toContainText(
      "default, then layer",
    );
    await expect(composer).toBeVisible();

    const createRequest = page.waitForRequest(
      (request) =>
        request.method() === "POST" &&
        request.url().endsWith(`/api/sessions/${session.id}/reviews`),
    );
    const commentRequest = page.waitForRequest(
      (request) =>
        request.method() === "POST" &&
        /\/api\/reviews\/\d+\/comments$/.test(request.url()),
    );
    await composer.getByRole("button", { name: "Add pending comment" }).click();
    const [create, comment] = await Promise.all([
      createRequest,
      commentRequest,
    ]);
    expect(create.postDataJSON().subject_version).toBe("1");
    expect(comment.postDataJSON().subject_version).toBe("1");
    await expect(page.getByTestId("review-tray")).toContainText("stale");
  });

  test("keeps review and long-document position through pop and dock", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "pop review",
      name: "review-pop",
    });
    const sections = Array.from(
      { length: 180 },
      (_, index) =>
        `## Section ${index}\n\nLong review body ${index}: context remains readable while scrolling.\n`,
    );
    const longDoc = `# Long design\n\n${sections.join("\n")}\nFinal review target at the end.\n`;
    await weaver.writeArtifact(session, "design", longDoc, {
      title: "Long design",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);

    await addPendingComment(
      page,
      "Long review body 0",
      "Opening context comment.",
    );
    const openingCard = page
      .locator('[data-testid^="review-comment-"]')
      .filter({ hasText: "Opening context comment." });
    await openingCard.click();
    const activeCard = page.locator("[data-review-card]");
    await activeCard.getByRole("button", { name: "Edit" }).click();
    await page
      .getByTestId("review-comment-edit")
      .fill("Opening context survives an in-flight layout swap.");
    await page.route(
      /\/api\/reviews\/\d+\/comments\/\d+$/,
      async (route) => {
        await new Promise((resolve) => setTimeout(resolve, 200));
        await route.continue();
      },
      { times: 1 },
    );
    await activeCard.getByRole("button", { name: "Save" }).click();
    await page.getByTestId("artifact-pop").click();
    await expect(page.getByTestId("artifact-pop")).toContainText("Dock");
    await expect(
      page.locator('[data-testid^="review-comment-"]').filter({
        hasText: "Opening context survives an in-flight layout swap.",
      }),
    ).toBeVisible();

    const scroller = page.getByTestId("artifact-scroll");
    await scroller.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
      element.dispatchEvent(new Event("scroll"));
    });
    const before = await scroller.evaluate((element) => ({
      top: element.scrollTop,
      max: element.scrollHeight - element.clientHeight,
    }));
    expect(before.max).toBeGreaterThan(4_000);
    expect(before.top).toBeGreaterThan(before.max * 0.9);
    await expect(page.getByText("Final review target")).toBeInViewport();

    await page.getByTestId("artifact-pop").click();
    const poppedScroller = page.getByTestId("artifact-scroll");
    await expect(poppedScroller).toBeVisible();
    await expect
      .poll(() => poppedScroller.evaluate((element) => element.scrollTop))
      .toBeGreaterThan(before.max * 0.75);
    await expect(page.getByText("Final review target")).toBeInViewport();

    await addPendingComment(
      page,
      "Final review target",
      "Closing context comment.",
    );
    await expect(page.getByTestId("review-tray")).toContainText("2 pending");

    await page.getByTestId("artifact-pop").click();
    await expect(page.getByTestId("artifact-scroll")).toBeVisible();
    await expect
      .poll(() =>
        page
          .getByTestId("artifact-scroll")
          .evaluate((element) => element.scrollTop),
      )
      .toBeGreaterThan(before.max * 0.7);
    await expect(page.getByText("Final review target")).toBeInViewport();
    await expect(page.getByTestId("review-tray")).toContainText("2 pending");

    await page.setViewportSize({ width: 760, height: 680 });
    const trayBox = await page.getByTestId("review-tray").boundingBox();
    expect(trayBox).not.toBeNull();
    expect(trayBox!.x).toBeGreaterThanOrEqual(0);
    expect(trayBox!.x + trayBox!.width).toBeLessThanOrEqual(760);
  });
});
