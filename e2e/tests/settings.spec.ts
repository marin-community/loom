import { test, expect } from "../fixtures/weaver";

test.describe("settings · profiles", () => {
  test("the default profile selects an agent, model, and effort", async ({
    page,
    weaver,
  }) => {
    const registry = (await (
      await fetch(`${weaver.baseUrl}/api/agents`)
    ).json()) as {
      agents: {
        kind: string;
        models: { id: string; label: string }[];
        efforts: { id: string; label: string }[];
      }[];
    };
    const codex = registry.agents.find((agent) => agent.kind === "codex")!;
    await page.goto(`${weaver.baseUrl}/settings`);

    const agent = page.getByTestId("profile-agent");
    await expect(agent.locator("option")).toContainText([
      "Claude",
      "Codex",
      "Shell",
    ]);

    await agent.selectOption("codex");
    await page.getByTestId("profile-model").fill(codex.models[0].id);
    await page.getByTestId("profile-effort").fill(codex.efforts[0].id);
    await page.getByTestId("profile-save").click();
    await expect(page.getByText("Saved default.")).toBeVisible();

    const saved = (await (
      await fetch(`${weaver.baseUrl}/api/profiles/default`)
    ).json()) as {
      agent_kind: string;
      model: string;
      effort: string;
    };
    expect(saved).toMatchObject({
      agent_kind: "codex",
      model: codex.models[0].id,
      effort: codex.efforts[0].id,
    });

    await expect(
      page.getByText("Fleet concierge runtime", { exact: true }),
    ).toHaveCount(0);
  });

  test("default profile permissions can be set to always allow", async ({
    page,
    weaver,
  }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    const mode = page.getByTestId("profile-mode");
    await expect(mode).toHaveValue("auto");
    await mode.selectOption("bypassPermissions");
    await page.getByTestId("profile-save").click();
    await expect(page.getByText("Saved default.")).toBeVisible();

    const saved = (await (
      await fetch(`${weaver.baseUrl}/api/profiles/default`)
    ).json()) as {
      mode: string;
    };
    expect(saved.mode).toBe("bypassPermissions");
    await expect(mode).toHaveValue("bypassPermissions");
  });

  test("overlapping settings are consolidated into workspace and access", async ({
    page,
    weaver,
  }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await expect(
      page.getByRole("button", { name: "Workspace", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Access", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Editor", exact: true }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Appearance", exact: true }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Authentication", exact: true }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Tokens", exact: true }),
    ).toHaveCount(0);
    await expect(
      page.getByRole("button", { name: "Account", exact: true }),
    ).toHaveCount(0);
    await expect(page.locator('[data-rail="chat"]')).toHaveCount(0);
  });
});
