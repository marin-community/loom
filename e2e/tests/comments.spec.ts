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
    await second.getByRole("button", { name: "Edit" }).click();
    await second
      .getByTestId("review-comment-edit")
      .fill("Explain prefix and suffix recovery.");
    await second.getByRole("button", { name: "Save" }).click();
    await expect(second).toContainText("Explain prefix and suffix recovery.");

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
    await staleCard.getByRole("button", { name: "Re-anchor" }).click();
    await selectPhrase(page, "staged review feedback");
    await page.getByTestId("review-selection-button").click();
    await expect(staleCard).toContainText("Revision 2");

    await page
      .getByTestId("review-overall-note")
      .fill("Address this before landing.");
    await expect(tray).toContainText("Conversation feedback preview");
    await expect(tray).toContainText("Address this before landing.");
    await expect(tray).toContainText(
      "This statement needs an explicit boundary.",
    );

    // The review began on revision 1; even after re-anchoring its comment, the
    // stale envelope requires explicit acknowledgement.
    await page.getByTestId("review-stale-ack").check();
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

    await page.getByTestId("artifact-pop").click();
    const poppedScroller = page.getByTestId("artifact-scroll");
    await expect(poppedScroller).toBeVisible();
    await expect
      .poll(() => poppedScroller.evaluate((element) => element.scrollTop))
      .toBeGreaterThan(before.max * 0.75);

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
    await expect(page.getByTestId("review-tray")).toContainText("2 pending");

    await page.setViewportSize({ width: 760, height: 680 });
    const trayBox = await page.getByTestId("review-tray").boundingBox();
    expect(trayBox).not.toBeNull();
    expect(trayBox!.x).toBeGreaterThanOrEqual(0);
    expect(trayBox!.x + trayBox!.width).toBeLessThanOrEqual(760);
  });
});
