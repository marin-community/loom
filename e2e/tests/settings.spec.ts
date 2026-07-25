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

    const agent = page.getByTestId("profile-agent");
    const model = page.getByTestId("profile-model");
    const effort = page.getByTestId("profile-effort");
    await expect(agent.locator("option")).toContainText([
      "Claude",
      "Codex",
      "Shell",
    ]);

    await agent.selectOption("claude");
    await model.selectOption(claude.models[0].id);
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
    await access.getByRole("button", { name: "groups" }).click();
    await access.getByLabel("docs").check();
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
    await access.getByRole("button", { name: "none" }).click();
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
