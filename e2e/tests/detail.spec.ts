import { test, expect } from "../fixtures/weaver";

test.describe("session detail view", () => {
  test("does not surface a failed background channel acknowledgement", async ({
    page,
    weaver,
  }) => {
    const session = await weaver.seedSession({
      goal: "Keep working",
      name: "ack-failure",
    });
    await page.route("**/api/channels/read_marker/set", async (route) => {
      const operands = route.request().postDataJSON() as { channel?: string };
      if (operands?.channel !== session.id) return route.fallback();
      await route.fulfill({
        status: 500,
        json: { error: "read marker failed" },
      });
    });

    await page.goto(`${weaver.baseUrl}/s/${session.id}`);
    await expect(
      page.getByRole("heading", { name: "ack-failure" }),
    ).toBeVisible();
    await expect(
      page.getByText(/couldn't acknowledge the channel/i),
    ).toHaveCount(0);
  });

  test("session exit commands are discoverable without stealing text input", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Leave the session from the keyboard",
      name: "keyboard-exit",
    });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    const hints = page.getByTestId("command-hints");
    await expect(hints).toContainText("back to sessions");
    await page.keyboard.press("Shift+/");
    const help = page.getByTestId("shortcut-help");
    await expect(
      help.locator('[data-command-id="session.back"]'),
    ).toBeVisible();
    await expect(
      help.locator('[data-command-id="session.tab.terminal"] + dd'),
    ).toContainText("Open Agent");
    await expect(
      help.locator('[data-command-id="session.tab.conversation"] + dd'),
    ).toContainText("Open Conversation");
    await expect(
      help.locator('[data-command-id="session.tab.review"] + dd'),
    ).toContainText("Open Review");
    await page.keyboard.press("Escape");
    await expect(help).toHaveCount(0);
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);

    const renameButton = page.getByRole("button", { name: "Rename" });
    await renameButton.focus();
    await page.keyboard.press("2");
    await expect(page.locator('[data-tab="conversation"]')).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await page.keyboard.press("3");
    await expect(page.locator('[data-tab="review"]')).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}/artifacts(?:/|$)`));
    await page.keyboard.press("c");
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}/changes`);
    await page.keyboard.press("a");
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}/artifacts(?:/|$)`));
    await page.keyboard.press("[");
    await expect(page.locator('[data-tab="conversation"]')).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await page.keyboard.press("]");
    await expect(page.locator('[data-tab="review"]')).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await page.keyboard.press("1");
    await expect(page.locator('[data-tab="terminal"]')).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);

    await renameButton.click();
    const titleInput = page.locator("header input").first();
    await titleInput.press("End");
    await titleInput.press("b");
    await expect(titleInput).toHaveValue("keyboard-exitb");
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);
    await titleInput.press("Escape");
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);

    await page.keyboard.press("b");
    await expect(page).toHaveURL(weaver.baseUrl + "/");

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByTestId("status-bar").click();
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(weaver.baseUrl + "/");
  });

  test("renders goal, status and identity metadata", async ({
    page,
    weaver,
  }) => {
    const goal = Array.from(
      { length: 40 },
      (_, index) => `Private operator context line ${index + 1}`,
    ).join("\n");
    const s = await weaver.seedSession({ goal, name: "detail-task" });
    await weaver.seedIssue(s, "Scoped from Details");

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    await expect(
      page.getByRole("heading", { name: "detail-task" }),
    ).toBeVisible();
    // Running is the silent lifecycle default — the header shows no lifecycle
    // badge for it (only off-nominal states get one, as on the fleet list).
    await expect(page.getByTestId("status-badge")).toHaveCount(0);

    await expect(page.getByRole("button", { name: "Overview" })).toHaveCount(0);

    // Identity and goal context live behind a keyboard-operable Details popover,
    // not in permanent header chrome.
    const trigger = page.getByRole("button", { name: /Details/ });
    await trigger.focus();
    await trigger.press("Enter");
    const details = page.getByTestId("details-popover");
    await expect(details.locator(":focus")).toHaveCount(1);
    await expect(details).toHaveAttribute("role", "region");
    await expect(details).not.toHaveAttribute("aria-modal", "true");
    await expect(details.getByText(s.id, { exact: true })).toBeVisible();
    await expect(
      details.getByText(s.branch.branch, { exact: true }),
    ).toBeVisible();
    await expect(
      details.getByText(`base ${s.branch.base_branch}`),
    ).toBeVisible();
    await expect(details.getByTestId("session-goal-context")).toBeHidden();
    await details.getByText("Goal / prompt", { exact: true }).click();
    const goalContext = details.getByTestId("session-goal-context");
    await expect(goalContext).toHaveText(goal);
    await expect(goalContext).toHaveCSS("overflow-y", "auto");
    const goalBounds = await goalContext.evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
    expect(goalBounds.scrollHeight).toBeGreaterThan(goalBounds.clientHeight);
    await expect(details.locator('a[href$="/artifacts/goal"]')).toHaveCount(0);

    // Details preserves a contextual Issues path, scoped by the literal branch
    // name (not the branch row id) for #630's claimed/source branch matching.
    const issuesLink = details.getByRole("link", { name: /\d+ open issues?/ });
    const issuesHref = await issuesLink.getAttribute("href");
    const issuesTarget = new URL(issuesHref!, weaver.baseUrl);
    expect(issuesTarget.pathname).toBe("/issues");
    expect(issuesTarget.searchParams.get("repo_root")).toBe(s.branch.repo_root);
    expect(issuesTarget.searchParams.get("branch")).toBe(s.branch.branch);

    // Escape dismisses the nonmodal popover and returns focus to its trigger.
    await page.keyboard.press("Escape");
    await expect(details).toHaveCount(0);
    await expect(trigger).toBeFocused();
  });

  test("outside dismissal preserves the focus target that was clicked", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Dismiss lightly",
      name: "outside-focus",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole("button", { name: /Details/ }).click();
    await expect(page.getByTestId("details-popover")).toBeVisible();

    const agentTab = page.locator('[data-tab="terminal"]');
    await agentTab.click();
    await expect(page.getByTestId("details-popover")).toHaveCount(0);
    await expect(agentTab).toBeFocused();
  });

  test("resource navigation closes Details across cached session routes", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Navigate cleanly",
      name: "details-navigation",
    });
    await weaver.seedIssue(s, "Scoped navigation");

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole("button", { name: /Details/ }).click();
    await page
      .getByTestId("details-popover")
      .getByRole("link", { name: "Artifacts" })
      .click();
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}/artifacts`));
    await expect(page.getByTestId("details-popover")).toHaveCount(0);

    await page.locator('[data-tab="terminal"]').click();
    await page.getByRole("button", { name: /Details/ }).click();
    await page
      .getByTestId("details-popover")
      .getByRole("link", { name: /\d+ open issues?/ })
      .click();
    await expect(page).toHaveURL(/\/issues\?/);
    await expect(page.getByTestId("details-popover")).toHaveCount(0);

    await page.goBack();
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page.getByTestId("details-popover")).toHaveCount(0);
  });

  test("Details preserves bounded, deduplicated surfaced links", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Keep operational links",
      name: "surfaced-links",
    });
    for (let index = 0; index < 13; index += 1) {
      await weaver.setStatus(
        s,
        "attention",
        `Review https://example.test/doc/${index} before continuing`,
      );
    }
    await weaver.setStatus(
      s,
      "attention",
      "Latest: https://example.test/doc/12.",
    );

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await page.getByRole("button", { name: /Details/ }).click();
    await page.getByText("Surfaced links (12)", { exact: true }).click();

    const links = page.getByTestId("session-links");
    await expect(links.getByRole("link")).toHaveCount(12);
    await expect(
      links.getByRole("link", { name: "example.test/doc/12" }),
    ).toHaveCount(1);
    await expect(
      links.getByRole("link", { name: "example.test/doc/12" }),
    ).toHaveAttribute("href", "https://example.test/doc/12");
    await expect(
      links.getByText("example.test/doc/0", { exact: true }),
    ).toHaveCount(0);
  });

  test("sets the browser tab title to the open session", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Name my tab",
      name: "tab-task",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    // The tab title tracks the open session (its title, falling back to the
    // branch name) so several loom tabs are tellable apart, composed centrally
    // as "Loom - <Section>". It's derived from the shared fleet snapshot, which
    // the deep link fills a beat after landing, so toHaveTitle auto-retries until
    // the row arrives.
    await expect(page).toHaveTitle("Loom - tab-task");

    // Leaving the session for the fleet list moves to the list's own section.
    await page.goto(`${weaver.baseUrl}/`);
    await expect(page).toHaveTitle("Loom - Sessions");
  });

  test("edits pull request and issue associations from visible pills", async ({
    page,
    weaver,
  }) => {
    const issue = await weaver.seedBacklogIssue(
      weaver.repoPath,
      "Map my issue",
    );
    const s = await weaver.seedSession({
      goal: "Map my PR",
      name: "pr-map",
      claimIssue: issue.id,
    });
    let requestBody: unknown;
    await page.route("**/api/sessions/github/set", async (route) => {
      requestBody = route.request().postDataJSON();
      await route.fulfill({
        json: { ...s, branch: { ...s.branch, github_pr: 37 } },
      });
    });
    await page.route("**/api/sessions/get", async (route) => {
      const operands = route.request().postDataJSON() as { session?: string };
      if (!requestBody || operands?.session !== s.id) return route.fallback();
      const response = await route.fetch();
      const current = (await response.json()) as typeof s;
      await route.fulfill({
        response,
        json: {
          ...current,
          github_repo: current.github_repo ?? "acme/widgets",
          branch: {
            ...current.branch,
            github_pr: 37,
            github: {
              pr_number: 37,
              pr_url: "https://github.com/acme/widgets/pull/37",
              pr_state: "OPEN",
              checks: "passing",
            },
          },
        },
      });
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const prPill = page.getByTestId("pr-association-pill");
    const issuePill = page.getByTestId("issue-association-pill");
    await expect(page.getByTestId("github-associations")).toBeVisible();
    await expect(prPill).toHaveText("PR —");
    await expect(issuePill).toHaveText("Issue —");

    await prPill.click();
    const form = page.getByTestId("pr-mapping-form");
    await form.getByLabel("PR number").fill("37");
    await form.getByRole("button", { name: "Pin PR" }).click();

    await expect
      .poll(() => requestBody)
      .toEqual({ pr_number: 37, session: s.id });
    await expect(form).toBeHidden();
    await expect(prPill).toHaveAttribute(
      "href",
      "https://github.com/acme/widgets/pull/37",
    );
    await expect(prPill).toHaveAttribute("target", "_blank");

    // Reassociation is secondary, but its popover follows the same Escape /
    // focus-return contract as Details.
    const prEdit = page.getByTestId("pr-association-edit");
    await prEdit.click();
    await expect(page.getByTestId("pr-mapping-popover")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId("pr-mapping-popover")).toHaveCount(0);
    await expect(prEdit).toBeFocused();

    await issuePill.click();
    const issueForm = page.getByTestId("issue-mapping-form");
    await issueForm.getByLabel("owner/repo#number").fill("acme/widgets#73");
    await issueForm.getByRole("button", { name: "Save" }).click();

    await expect(issuePill).toHaveText("Issue #73");
    await expect
      .poll(async () => (await weaver.getSession(s.id)).github_issue)
      .toEqual({
        repo: "acme/widgets",
        number: 73,
      });
    await expect(issuePill).toHaveAttribute(
      "href",
      "https://github.com/acme/widgets/issues/73",
    );

    await page.getByTestId("issue-association-edit").click();
    await page
      .getByTestId("issue-mapping-form")
      .getByRole("button", { name: "Clear" })
      .click();
    await expect(issuePill).toHaveText("Issue —");
  });

  test("viewing a session acknowledges current attention and later signals can return", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Acknowledge me",
      name: "ack-task",
    });
    // Agent and watch signals are both current when the user enters.
    await weaver.setStatus(s, "attention", "waiting on review");
    await weaver.mark(s, "blocked", {
      note: "external assessment",
      by: "status-watch",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    // Opening is the acknowledgement gesture: current loud tags disappear
    // server-side, while the durable status message remains as recent context.
    await expect
      .poll(async () =>
        (await weaver.getSession(s.id)).branch.tags.filter((tag) =>
          ["attention", "blocked"].includes(tag.value),
        ),
      )
      .toEqual([]);
    await expect(
      page.getByText("waiting on review", { exact: true }),
    ).toBeVisible();
    await expect(page.getByTestId("status-bar-attention")).toContainText(
      "no new attention",
    );

    // A signal raised after the page was opened is not continuously erased.
    await weaver.setStatus(s, "attention", "new decision needed");
    const chip = page.locator(
      '[data-testid="signal-chip"][data-signal-key="attention"]',
    );
    await expect(chip).toHaveAttribute("data-level", "attention");

    // Leaving and returning is a new acknowledgement.
    await page.goto(weaver.baseUrl);
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect
      .poll(async () =>
        (await weaver.getSession(s.id)).branch.tags.some(
          (tag) => tag.key === "attention",
        ),
      )
      .toBe(false);
  });

  test("scratch attachments share one bounded browse and drop target", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Hold my files",
      name: "scratch-task",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    const panel = page.getByTestId("scratch-panel");
    await expect(panel.getByRole("button", { name: "Attach" })).toBeVisible();

    // The Attach affordance drives a hidden file input.
    await panel.locator("input[type=file]").setInputFiles({
      name: "notes.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("hello"),
    });
    await expect(panel.getByText("notes.txt")).toBeVisible();

    // The same bounded component accepts a drop from files.length even when
    // DataTransfer.types does not advertise Files.
    const dataTransfer = await page.evaluateHandle(() => {
      const dt = new DataTransfer();
      dt.items.add(new File(["drop"], "dropped.txt", { type: "text/plain" }));
      Object.defineProperty(dt, "types", { value: ["text/plain"] });
      return dt;
    });
    await page
      .getByTestId("scratch-dropzone")
      .dispatchEvent("drop", { dataTransfer });
    const scratchMenuButton = panel.getByRole("button", {
      name: "Scratch files, 2 attached",
    });
    await expect(scratchMenuButton).toBeVisible();
    await scratchMenuButton.click();
    const scratchMenu = panel.getByTestId("scratch-menu");
    await expect(scratchMenu.getByText("dropped.txt")).toBeVisible();

    // Both landed server-side in the worktree's scratch/.
    const res = await fetch(`${weaver.baseUrl}/api/sessions/scratch/list`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ session: s.id }),
    });
    const listed = ((await res.json()) as { name: string }[])
      .map((f) => f.name)
      .sort();
    expect(listed).toEqual(["dropped.txt", "notes.txt"]);

    // The collection menu's ✕ removes that file.
    await scratchMenu
      .getByRole("menuitem", { name: "Remove notes.txt" })
      .click();
    await expect(panel.getByText("notes.txt")).toHaveCount(0);
  });

  test("narrow session chrome keeps scratch files and navigation bounded", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Keep a phone-sized workbench usable",
      name: "mobile-scratch-task",
    });
    const names = Array.from(
      { length: 8 },
      (_, index) =>
        `20260816-${String(index + 1).padStart(4, "0")}-monitoring-state.json`,
    );
    for (const name of names) {
      const response = await fetch(
        `${weaver.baseUrl}/api/sessions/scratch/write?session=${encodeURIComponent(s.id)}&name=${encodeURIComponent(name)}`,
        { method: "POST", body: Buffer.from("{}") },
      );
      expect(response.ok, await response.text()).toBe(true);
    }

    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    // The phone bar keeps only the three durable destinations visible; every
    // lower-frequency session surface moves behind More.
    const primary = page.getByRole("navigation", { name: "Primary" });
    const primaryBox = await primary.boundingBox();
    expect(primaryBox).not.toBeNull();
    expect(primaryBox!.width).toBe(390);
    expect(primaryBox!.y).toBeGreaterThan(780);
    for (const label of ["Sessions", "Chat", "Artifacts", "More"]) {
      await expect(primary.getByText(label, { exact: true })).toBeVisible();
    }
    await expect(primary.getByText("Agent", { exact: true })).toHaveCount(0);
    await expect(primary.getByText("Changes", { exact: true })).toHaveCount(0);
    await expect(primary.getByRole("button", { name: "Chat" })).toHaveAttribute(
      "aria-current",
      "page",
    );

    // The old top tab row is gone on phones. The compact header leaves the
    // title and live state visible without duplicating bottom navigation.
    await expect(page.getByTestId("session-tabs")).toBeHidden();
    const headerBox = await page.getByTestId("session-header").boundingBox();
    expect(headerBox).not.toBeNull();
    expect(headerBox!.height).toBeLessThan(70);

    // Scratch, Details, and global escapes live in one bounded More sheet.
    const moreButton = primary.getByRole("button", { name: "More" });
    await moreButton.click();
    const more = page.getByTestId("mobile-more-menu");
    const mobileDetails = more.getByTestId("mobile-session-details");
    const mobileSettings = more.getByRole("link", { name: "Settings" });
    await expect(more.getByTestId("mobile-session-agent")).toBeVisible();
    await expect(more.getByTestId("mobile-session-changes")).toBeVisible();
    await expect(mobileDetails).toBeFocused();
    await page.keyboard.press("Shift+Tab");
    await expect(mobileSettings).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(mobileDetails).toBeFocused();
    const scratch = more.getByTestId("mobile-scratch-panel");
    const scratchButton = scratch.getByRole("button", {
      name: "Scratch files, 8 attached",
    });
    await expect(scratchButton).toBeVisible();
    await scratchButton.click();
    const menu = scratch.getByTestId("scratch-menu");
    await expect(menu).toBeVisible();
    await expect(menu.getByText(names[0], { exact: true })).toBeVisible();
    const menuBox = await menu.boundingBox();
    expect(menuBox).not.toBeNull();
    expect(menuBox!.x).toBeGreaterThanOrEqual(0);
    expect(menuBox!.x + menuBox!.width).toBeLessThanOrEqual(390);

    await menu.getByRole("menuitem", { name: `Remove ${names[0]}` }).click();
    await expect(
      scratch.getByRole("button", { name: "Scratch files, 7 attached" }),
    ).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(menu).toHaveCount(0);
    await expect(
      scratch.getByRole("button", { name: "Scratch files, 7 attached" }),
    ).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(more).toHaveCount(0);
    await expect(moreButton).toBeFocused();

    // Session Details reopens from More as a phone-width bottom sheet.
    await moreButton.click();
    await page.getByTestId("mobile-session-details").click();
    const details = page.getByTestId("details-popover");
    await expect(details).toBeVisible();
    const detailsBox = await details.boundingBox();
    expect(detailsBox).not.toBeNull();
    expect(detailsBox!.x).toBeGreaterThanOrEqual(0);
    expect(detailsBox!.x + detailsBox!.width).toBeLessThanOrEqual(390);
    await page.keyboard.press("Escape");

    // Work-surface switches stay within the warm SessionDetail instance.
    await primary.getByRole("button", { name: "Artifacts" }).click();
    await expect(page).toHaveURL(new RegExp(`/s/${s.id}/artifacts`));

    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= window.innerWidth,
      ),
    ).toBe(true);

    await primary.getByRole("link", { name: "Sessions" }).click();
    await expect(page).toHaveURL(weaver.baseUrl + "/");
  });

  test("ACP phone navigation keeps Shells in More instead of the primary bar", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Keep a headless session focused on conversation",
      name: "mobile-acp-navigation",
    });
    await page.route("**/api/sessions/get", async (route) => {
      const operands = route.request().postDataJSON() as { session?: string };
      if (operands?.session !== s.id) return route.fallback();
      const response = await route.fetch();
      const session = (await response.json()) as Record<string, unknown>;
      await route.fulfill({ response, json: { ...session, protocol: "acp" } });
    });

    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    const primary = page.getByRole("navigation", { name: "Primary" });
    await expect(primary.getByText("Sessions", { exact: true })).toBeVisible();
    for (const label of ["Chat", "Artifacts", "More"]) {
      await expect(primary.getByRole("button", { name: label })).toBeVisible();
    }
    await expect(primary.getByRole("button", { name: "Changes" })).toHaveCount(
      0,
    );
    await expect(primary.getByRole("button", { name: "Shells" })).toHaveCount(
      0,
    );

    await primary.getByRole("button", { name: "More" }).click();
    const shells = page.getByTestId("mobile-session-shells");
    await expect(shells).toBeVisible();
    await expect(page.getByTestId("mobile-session-agent")).toHaveCount(0);
    await expect(page.getByTestId("mobile-session-changes")).toBeVisible();
    await shells.click();
    await expect(page.getByTestId("acp-open-shell")).toBeVisible();
    await expect(primary.getByRole("button", { name: "More" })).toHaveClass(
      /text-fg/,
    );
  });

  test("renders an interactive terminal that connects to the agent", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Receive a command",
      name: "term-task",
    });

    await page.goto(`${weaver.baseUrl}/s/${s.id}`);

    // The xterm.js terminal mounts.
    await expect(page.locator(".xterm")).toBeVisible();
    await expect(page.locator(".xterm-screen")).toBeVisible();

    // It connects: the connection-state overlay (connecting/reconnecting/
    // disconnected) clears once the WebSocket reaches the PTY. This is
    // renderer-independent; the keystroke→PTY→output byte round-trip itself is
    // covered deterministically by the Rust integration test (WebGL draws to a
    // canvas, so asserting rendered text here would be renderer-dependent).
    await expect(page.getByTestId("term-status")).toHaveCount(0, {
      timeout: 20_000,
    });
  });

  test("returns to the end of terminal scrollback after leaving a session", async ({
    page,
    weaver,
  }) => {
    const s = await weaver.seedSession({
      goal: "Keep the latest output in view",
      name: "term-return",
    });
    await page.goto(`${weaver.baseUrl}/s/${s.id}`);
    await expect(page.getByTestId("term-status")).toHaveCount(0, {
      timeout: 20_000,
    });

    const sent = await fetch(`${weaver.baseUrl}/api/sessions/send`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ text: "seq 1 200", submit: true, session: s.id }),
    });
    expect(sent.ok).toBe(true);

    // xterm 6 uses a model-backed custom scrollbar rather than native
    // scrollTop. Its slider position is the rendered view of that model.
    const scrollable = page.locator(".xterm-scrollable-element");
    const scrollbar = scrollable.locator("> .scrollbar.vertical");
    const slider = scrollbar.locator("> .slider");
    const sliderPosition = () =>
      slider.evaluate((element) => ({
        top: (element as HTMLElement).offsetTop,
        max:
          (element.parentElement as HTMLElement).clientHeight -
          (element as HTMLElement).offsetHeight,
      }));
    await expect(scrollbar).toHaveClass(/visible/);

    await scrollable.hover();
    await page.mouse.wheel(0, -10_000);
    await expect
      .poll(async () => (await sliderPosition()).top)
      .toBeLessThan(10);

    await page.locator('[data-rail="sessions"]').click();
    await page.goBack();
    await expect(page).toHaveURL(`${weaver.baseUrl}/s/${s.id}`);
    await expect
      .poll(async () => {
        const { top, max } = await sliderPosition();
        return max - top;
      })
      .toBeLessThan(10);
  });
});
