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

async function selectPhrase(page: Page, phrase: string) {
  await page.evaluate((needle) => {
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
    if (!node) throw new Error(`phrase not found: ${needle}`);
    const range = document.createRange();
    range.setStart(node, index);
    range.setEnd(node, index + needle.length);
    const selection = window.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
    body.dispatchEvent(new MouseEvent("mouseup", { bubbles: true }));
  }, phrase);
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test.describe("staged artifact reviews", () => {
  test("drafts, conflicts, re-anchors, and submits one coherent review", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "review lifecycle",
      name: "review-lifecycle",
    });
    await weaver.writeArtifact(session, "design", DOC, {
      title: "Design notes",
    });
    await weaver.writeArtifact(session, "alpha", "# Alpha identity\n", {
      title: "Alpha identity",
    });
    await weaver.writeArtifact(session, "beta", "# Beta identity\n", {
      title: "Beta identity",
    });
    await page.goto(`${weaver.baseUrl}/s/${session.id}/artifacts/design`);
    const article = page.locator(".markdown-body");
    await expect(article.locator("h1")).toContainText("Design notes");

    let releaseAlpha!: () => void;
    const alphaGate = new Promise<void>((resolve) => {
      releaseAlpha = resolve;
    });
    await page.route(
      `**/api/sessions/${session.id}/artifacts/alpha`,
      async (route) => {
        await alphaGate;
        await route.continue();
      },
      { times: 1 },
    );
    const alphaResponse = page.waitForResponse(
      (response) =>
        response.request().method() === "GET" &&
        response.url().endsWith(`/api/sessions/${session.id}/artifacts/alpha`),
    );
    await page.locator('[data-artifact="alpha"]').click();
    await expect(page.getByText("loading…")).toBeVisible();
    await expect(page.locator(".markdown-body")).toHaveCount(0);
    await expect(page.getByTestId("artifact-source-editor")).toHaveCount(0);
    await page.locator('[data-artifact="beta"]').click();
    await expect(page.locator(".markdown-body h1")).toContainText(
      "Beta identity",
    );
    releaseAlpha();
    const completedAlphaResponse = await alphaResponse;
    await completedAlphaResponse.finished();
    await page.evaluate(
      () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve())),
    );
    await expect(page.locator(".markdown-body h1")).toContainText(
      "Beta identity",
    );
    await page.locator('[data-artifact="design"]').click();
    await expect(article.locator("h1")).toContainText("Design notes");

    // Exercise the real keyboard selection path once; later selections use the
    // same browser Selection API without duplicating keyboard mechanics.
    await article.focus();
    for (let index = 0; index < 6; index += 1) {
      await page.keyboard.press("Shift+ArrowRight");
    }
    await page.keyboard.press("Tab");
    await page.keyboard.press("Enter");
    const composer = page.getByTestId("review-comment-composer");
    await expect(composer.locator("textarea")).toBeFocused();
    await composer
      .locator("textarea")
      .fill("Keep the document title aligned with the review.");
    const firstComment = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        /\/api\/reviews\/\d+\/comments$/.test(response.url()),
    );
    await composer.getByRole("button", { name: "Add pending comment" }).click();
    await firstComment;
    const initialReviews = await page.request.get(
      `${weaver.baseUrl}/api/sessions/${session.id}/reviews?subject_kind=artifact&subject_key=design`,
    );
    const initialDraft = (
      (await initialReviews.json()) as Array<{
        id: number;
        draft_revision: number;
      }>
    )[0];
    const secondComment = await page.request.post(
      `${weaver.baseUrl}/api/reviews/${initialDraft.id}/comments`,
      {
        data: {
          expected_revision: initialDraft.draft_revision,
          subject_version: "1",
          anchor_kind: "text",
          anchor: {
            quote: "How do anchors survive",
            prefix: "- ",
            suffix: " an edit elsewhere in the document?",
            block_index: 3,
          },
          body: "Explain the captured context used during drift.",
        },
      },
    );
    expect(secondComment.ok()).toBeTruthy();
    await page.reload();

    const tray = page.getByTestId("review-tray");
    await expect(tray).toContainText("2 pending");
    const anchoredMatch = page
      .locator('[data-testid^="review-comment-"]')
      .filter({ hasText: "captured context" });
    const anchoredId = await anchoredMatch.getAttribute("data-testid");
    const anchored = page.getByTestId(anchoredId!);
    await anchored.click();
    await anchored.getByRole("button", { name: "Edit" }).click();
    await anchored
      .getByTestId("review-comment-edit")
      .fill("Explain prefix and suffix recovery.");
    await anchored.getByRole("button", { name: "Save" }).click();
    await expect(anchored).toContainText("Explain prefix and suffix recovery.");
    await page.keyboard.press("Escape");

    const titleComment = page
      .locator('[data-testid^="review-comment-"]')
      .filter({ hasText: "document title" });
    await titleComment.click();
    await titleComment.getByRole("button", { name: "Delete" }).click();
    await expect(titleComment).toContainText("Delete this pending comment?");
    await titleComment.getByRole("button", { name: "Delete comment" }).click();
    await page.getByTestId("review-tray-toggle").click();
    await expect(tray).toContainText("1 pending");

    const note = page.getByTestId("review-overall-note");
    const initialSave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await note.fill("Initial overall guidance.");
    await note.blur();
    await initialSave;

    await page.reload();
    await page.getByTestId("review-tray-toggle").click();
    await expect(page.getByTestId("review-overall-note")).toHaveValue(
      "Initial overall guidance.",
    );
    await expect(
      page
        .locator('[data-testid^="review-comment-"]')
        .filter({ hasText: "Explain prefix and suffix recovery." }),
    ).toHaveCount(1);

    const listed = await page.request.get(
      `${weaver.baseUrl}/api/sessions/${session.id}/reviews?subject_kind=artifact&subject_key=design`,
    );
    const draft = (
      (await listed.json()) as Array<{ id: number; draft_revision: number }>
    )[0];
    const external = await page.request.patch(
      `${weaver.baseUrl}/api/reviews/${draft.id}`,
      {
        data: {
          expected_revision: draft.draft_revision,
          summary: "Newer server-side guidance.",
        },
      },
    );
    expect(external.ok()).toBeTruthy();

    const conflict = page.waitForResponse(
      (response) =>
        response.status() === 409 &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await page
      .getByTestId("review-overall-note")
      .fill("Conflicting stale guidance.");
    await page.getByTestId("review-overall-note").blur();
    await conflict;
    await expect(page.getByRole("alert")).toContainText(
      "draft changed elsewhere",
    );
    await expect(page.getByTestId("review-overall-note")).toHaveValue(
      "Conflicting stale guidance.",
    );

    const finalSave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await page
      .getByTestId("review-overall-note")
      .fill("Address this before landing.");
    await page.getByTestId("review-overall-note").blur();
    await finalSave;

    const revised = DOC.replace(
      "How do anchors survive",
      "How can anchors survive",
    );
    await weaver.writeArtifact(session, "design", revised, {
      title: "Design notes",
    });
    await expect(article).toContainText("How can anchors survive");
    await expect(page.getByTestId("review-stale-warning")).toBeVisible();
    await tray
      .getByRole("button", { name: /Explain prefix and suffix recovery/ })
      .click();
    const staleCard = page.locator("[data-review-card]");
    await staleCard.getByRole("button", { name: "Re-anchor" }).click();
    await selectPhrase(page, "How can anchors survive");
    await page.getByTestId("review-selection-button").click();
    await expect(staleCard).toContainText("How can anchors survive");
    await expect(page.getByTestId("review-stale-warning")).toBeHidden();
    await expect(tray).toContainText("Address this before landing.");
    await expect(tray).toContainText("Explain prefix and suffix recovery.");

    const submit = page.waitForRequest(
      (request) =>
        request.method() === "POST" &&
        /\/api\/reviews\/\d+\/submit$/.test(request.url()),
    );
    await page.getByTestId("submit-review").click();
    await submit;
    await expect(tray).toContainText("Review submitted");
    await expect(page.getByTestId("submit-review")).toHaveCount(0);
  });

  test("preserves a long document and in-flight overall draft through pop and dock", async ({
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
    await page.getByTestId("review-tray-toggle").click();
    const createGate = deferred();
    const createStarted = deferred();
    let createGated = false;
    await page.route(`**/api/sessions/${session.id}/reviews`, async (route) => {
      if (route.request().method() !== "POST" || createGated) {
        await route.continue();
        return;
      }
      createGated = true;
      createStarted.resolve();
      await createGate.promise;
      await route.continue();
    });
    await page
      .getByTestId("review-overall-note")
      .fill("Overall feedback survives the layout swap.");

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
    const create = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        response.url().endsWith(`/api/sessions/${session.id}/reviews`),
    );
    await page.getByTestId("artifact-pop").click();
    await createStarted.promise;
    await expect(page.getByTestId("artifact-pop")).toContainText("Pop out");
    await expect(page.getByTestId("artifact-rail-close")).toHaveCount(0);
    await expect(page.getByTestId("review-layout-barrier")).toBeVisible();
    await expect(page.getByTestId("review-surface")).toHaveAttribute(
      "inert",
      "",
    );
    await expect(page.getByTestId("review-overall-note")).toBeDisabled();
    const frozenValue = await page
      .getByTestId("review-overall-note")
      .inputValue();
    await page
      .getByTestId("review-overall-note")
      .evaluate((element: HTMLTextAreaElement) => element.focus());
    await page.keyboard.type("must not enter the frozen review");
    await expect(page.getByTestId("review-overall-note")).toHaveValue(
      frozenValue,
    );
    expect(
      await page.evaluate(() =>
        document.activeElement?.getAttribute("data-testid"),
      ),
    ).not.toBe("review-overall-note");
    createGate.resolve();
    await create;
    await expect(page.getByTestId("artifact-pop")).toContainText("Dock");
    await expect
      .poll(() =>
        page
          .getByTestId("artifact-scroll")
          .evaluate((element) => element.scrollTop),
      )
      .toBeGreaterThan(before.max * 0.75);
    await page.getByTestId("review-tray-toggle").click();
    await expect(page.getByTestId("review-overall-note")).toHaveValue(
      "Overall feedback survives the layout swap.",
    );

    await page.getByTestId("artifact-pop").click();
    await expect(page.getByTestId("artifact-pop")).toContainText("Pop out");
    await expect
      .poll(() =>
        page
          .getByTestId("artifact-scroll")
          .evaluate((element) => element.scrollTop),
      )
      .toBeGreaterThan(before.max * 0.7);
    await expect(page.getByText("Final review target")).toBeInViewport();

    await selectPhrase(page, "Final review target at the end.");
    await page.getByTestId("review-selection-button").click();
    const pendingComposer = page.getByTestId("review-comment-composer");
    const pendingText = pendingComposer.locator("textarea");
    await pendingText.fill("Keep this local composer text.");
    await page.getByTestId("artifact-pop").click();
    await expect(page.getByTestId("artifact-pop")).toContainText("Pop out");
    await expect(pendingComposer.getByRole("alert")).toContainText(
      "Add or cancel this pending comment",
    );
    await expect(pendingText).toBeFocused();
    await pendingComposer.getByRole("button", { name: "Cancel" }).click();

    await selectPhrase(page, "Final review target at the end.");
    await page.getByTestId("review-selection-button").click();
    await pendingComposer.locator("textarea").fill("Saved comment body.");
    await pendingComposer.getByRole("button", { name: "Add pending comment" }).click();
    const commentCard = page.locator("[data-review-card]").first();
    await commentCard.getByRole("button", { name: "Edit", exact: true }).click();
    const commentEdit = commentCard.getByTestId("review-comment-edit");
    await commentEdit.fill("Unsaved existing-comment edit.");
    await page.getByTestId("artifact-pop").click();
    await expect(page.getByTestId("artifact-pop")).toContainText("Pop out");
    await expect(commentCard.getByRole("alert")).toContainText("Save or cancel this comment edit");
    await expect(commentEdit).toBeFocused();
    await commentCard.getByRole("button", { name: "Cancel" }).click();

    const patchStarted = deferred();
    const patchGate = deferred();
    let failPatches = true;
    let firstFailedPatch = true;
    await page.route("**/api/reviews/*", async (route) => {
      if (
        route.request().method() !== "PATCH" ||
        !/\/api\/reviews\/\d+$/.test(route.request().url()) ||
        !failPatches
      ) {
        await route.continue();
        return;
      }
      if (firstFailedPatch) {
        firstFailedPatch = false;
        patchStarted.resolve();
        await patchGate.promise;
      }
      await route.fulfill({
        status: 500,
        contentType: "application/json",
        body: JSON.stringify({ error: "injected route save failure" }),
      });
    });
    const trayToggle = page.getByTestId("review-tray-toggle");
    if ((await trayToggle.getAttribute("aria-expanded")) !== "true") {
      await trayToggle.click();
    }
    const reviewNote = page.getByTestId("review-overall-note");
    const artifactUrl = `${weaver.baseUrl}/s/${session.id}/artifacts/design`;

    await reviewNote.fill("Route feedback must block a failed leave.");
    await page.locator('[data-rail="issues"]').click();
    await patchStarted.promise;
    await expect(page).toHaveURL(artifactUrl);
    await expect(reviewNote).toBeDisabled();
    patchGate.resolve();
    await expect(page.getByRole("alert")).toContainText(
      "injected route save failure",
    );
    await expect(page).toHaveURL(artifactUrl);
    await expect(reviewNote).toBeEnabled();

    failPatches = false;
    const successfulSave = page.waitForResponse(
      (response) =>
        response.request().method() === "PATCH" &&
        /\/api\/reviews\/\d+$/.test(response.url()),
    );
    await reviewNote.fill("Route feedback survives app-rail navigation.");
    await page.locator('[data-rail="issues"]').click();
    await successfulSave;
    await expect(page).toHaveURL(`${weaver.baseUrl}/issues`);

    await page.goBack();
    await expect(page).toHaveURL(artifactUrl);
    if ((await trayToggle.getAttribute("aria-expanded")) !== "true") {
      await trayToggle.click();
    }
    await expect(reviewNote).toHaveValue(
      "Route feedback survives app-rail navigation.",
    );
  });
});
