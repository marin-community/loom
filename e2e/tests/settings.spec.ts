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
    const claude = registry.agents.find((agent) => agent.kind === "claude")!;
    const codex = registry.agents.find((agent) => agent.kind === "codex")!;
    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId("settings-category-agents").click();

    const agent = page.getByTestId("profile-agent");
    const model = page.getByTestId("profile-model");
    const effort = page.getByTestId("profile-effort");
    await expect(agent.locator("option")).toContainText([
      "Claude",
      "Codex",
      "Shell",
    ]);

    await agent.selectOption("claude");
    await model.click();
    await model.fill(claude.models[0].id);
    await page.keyboard.press("Enter");
    await agent.selectOption("codex");
    await expect(model).toHaveValue("");
    await expect(model.locator("option")).toContainText([
      "Agent default",
      ...codex.models.map((choice) => choice.label),
    ]);
    await expect(effort.locator("option")).toContainText([
      "Agent default",
      ...codex.efforts.map((choice) => choice.label),
    ]);
    await model.selectOption(codex.models[0].id);
    await effort.selectOption(codex.efforts[0].id);
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
    await page.getByTestId("settings-category-agents").click();
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

  test("custom MCP source validates and becomes a selectable profile group", async ({
    page,
    weaver,
  }) => {
    const source = `# /// script
# requires-python = ">=3.11"
# ///
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {
            "protocolVersion": request["params"]["protocolVersion"],
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "docs-search", "version": "1"},
        }
        response = {"jsonrpc": "2.0", "id": request["id"], "result": result}
    elif method == "tools/list":
        tool = {
            "name": "lookup",
            "description": "Search the docs",
            "inputSchema": {"type": "object", "properties": {}},
        }
        response = {
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": {"tools": [tool]},
        }
    else:
        response = {
            "jsonrpc": "2.0",
            "id": request["id"],
            "error": {"code": -32601, "message": "not found"},
        }
    print(json.dumps(response), flush=True)
`;

    await page.goto(`${weaver.baseUrl}/settings`);
    await page.getByTestId("settings-category-agents").click();
    const panel = page.getByTestId("mcp-panel");
    await panel.getByRole("button", { name: "Add custom MCP" }).click();
    await panel.getByLabel("Identity").fill("/docs/search");
    await panel.getByLabel("Label").fill("Docs search");
    await panel.getByLabel("Python MCP source (PEP 723)").fill(source);
    await panel.getByRole("button", { name: "Save and validate" }).click();
    await expect(panel.getByText("ready · r1")).toBeVisible({
      timeout: 30_000,
    });

    const custom = (await (
      await fetch(`${weaver.baseUrl}/api/mcps/custom/docs/search`)
    ).json()) as {
      identity: string;
      group: string;
      tools: string[];
      validation_state: string;
    };
    expect(custom).toMatchObject({
      identity: "/docs/search",
      group: "docs",
      tools: ["lookup"],
      validation_state: "ready",
    });

    await page.reload();
    await page.getByTestId("profile-agent").selectOption("codex");
    await page.getByLabel("Protocol").selectOption("acp");
    const access = page.getByRole("group", { name: "MCP access" });
    await access.getByRole("radio", { name: "groups" }).check();
    await access.getByLabel("docs").check();
    await expect(
      access.getByLabel("docs").locator("..").getByText("1 service · 1 tool"),
    ).toBeVisible();
    await expect(access.getByText("New sessions get 1 tool")).toBeVisible();
    await page.getByTestId("profile-save").click();
    await expect(page.getByText("Saved default.")).toBeVisible();

    const profile = (await (
      await fetch(`${weaver.baseUrl}/api/profiles/default`)
    ).json()) as {
      mcp_access: { mode: string; groups: string[] };
    };
    expect(profile.mcp_access).toEqual({ mode: "groups", groups: ["docs"] });

    // This worker reuses one isolated server. Restore the shared default
    // template so a later terminal seed is not intentionally rejected by the
    // terminal + MCP cross-field invariant.
    await access.getByRole("radio", { name: "none" }).check();
    await page.getByTestId("profile-save").click();
    await expect(page.getByText("Saved default.")).toBeVisible();
    const removed = await fetch(
      `${weaver.baseUrl}/api/mcps/custom/docs/search`,
      {
        method: "DELETE",
      },
    );
    expect(removed.ok).toBe(true);
  });

  test("settings separate personal use from deployment administration", async ({
    page,
    weaver,
  }) => {
    await page.goto(`${weaver.baseUrl}/settings`);
    await expect(
      page.getByRole("button", { name: "Account", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Preferences", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Diagnostics", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "People & security", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Agents & profiles", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Integrations", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Runtime", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Automation", exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("complementary").getByText("Personal", { exact: true }),
    ).toBeVisible();
    await expect(page.getByText("Operations", { exact: true })).toBeVisible();
    await expect(
      page.getByText("Administration", { exact: true }),
    ).toBeVisible();
    await expect(page.locator('[data-rail="chat"]')).toHaveCount(0);

    await expect(
      page.getByText("Your GitHub token", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("Loom GitHub App", { exact: true }),
    ).toHaveCount(0);
    const createToken = page.getByRole("link", { name: "Create one" });
    await expect(createToken).toHaveAttribute("href", /contents=write/);
    await expect(createToken).toHaveAttribute("href", /issues=write/);
    await expect(createToken).toHaveAttribute("href", /pull_requests=write/);

    await page
      .getByRole("button", { name: "Integrations", exact: true })
      .click();
    await expect(page.getByTestId("github-connection-panel")).toBeVisible();
    await expect(
      page.getByText("Loom GitHub App", { exact: true }),
    ).toBeVisible();
    await expect(
      page.getByText("Your GitHub token", { exact: true }),
    ).toHaveCount(0);

    await page
      .getByRole("button", { name: "Agents & profiles", exact: true })
      .click();
    expect(
      await page.getByTestId("metadata-settings").evaluate((section) => ({
        settingsVisible: [
          "metadata.title_generation",
          "metadata.resumption_cues",
          "metadata.resumption_inactivity_secs",
          "metadata.allow_restricted",
        ].every((key) => section.textContent?.includes(key)),
        metadataProfileVisible:
          section.textContent?.includes("metadata.profile"),
      })),
    ).toEqual({
      settingsVisible: true,
      metadataProfileVisible: false,
    });
  });

  test("users see personal settings and diagnostics without admin controls", async ({
    page,
    weaver,
  }) => {
    await page.route("**/api/auth/me", async (route) => {
      await route.fulfill({
        status: 200,
        contentType: "application/json",
        body: JSON.stringify({
          authenticated: true,
          username: "alice",
          github_login: "alice-gh",
          via: "token",
          role: "user",
          methods: { password: true, github: false },
        }),
      });
    });

    await page.goto(`${weaver.baseUrl}/settings?tab=integrations`);
    await expect(page).toHaveURL(`${weaver.baseUrl}/settings`);
    await expect(page.getByTestId("settings-category-account")).toBeVisible();
    await expect(
      page.getByTestId("settings-category-preferences"),
    ).toBeVisible();
    await expect(
      page.getByTestId("settings-category-diagnostics"),
    ).toBeVisible();
    await expect(page.getByText("Administration", { exact: true })).toHaveCount(
      0,
    );
    await expect(page.getByTestId("settings-category-people")).toHaveCount(0);
    await expect(page.getByTestId("settings-category-agents")).toHaveCount(0);
    await expect(page.getByText("alice", { exact: true })).toBeVisible();
    await expect(
      page.getByText("User · via token", { exact: false }),
    ).toBeVisible();
    await expect(page.locator('[data-rail="shell"]')).toHaveCount(0);

    await page.goto(`${weaver.baseUrl}/shell`);
    await expect(page).toHaveURL(`${weaver.baseUrl}/`);
  });
});
