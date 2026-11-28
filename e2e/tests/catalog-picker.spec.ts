import { test, expect } from '../fixtures/weaver';
import type { WeaverFixture } from '../fixtures/weaver';

// The model picker's Vue wiring — ProfileEditor and LaunchOverrides binding
// their ModelComboboxes to the catalogue `agents.list` serves, and the mode
// list narrowing when the agent changes. The Rust journey
// (custom_agents::a_catalogued_model_effort_and_mode_save_and_resolve) covers
// the API tiers; this one drives the browser against the same contract.
//
// Every worker's builtin ACP commands are pinned to the fake adapter
// (fixtures/weaver.ts), so the catalogue this spec refreshes is deterministic:
// the fake advertises fake-fast/fake-deep with low/medium/high effort and the
// boolean fast toggle, whatever harnesses the machine really has installed.

const HEADERS = { 'content-type': 'application/json' };

/** A catalogued agent as `agents.list` serves it (the fields this spec reads). */
interface CataloguedAgent {
  kind: string;
  models: {
    id: string;
    label: string;
    efforts: { id: string; label: string }[];
    fast: { id: string; label: string }[];
  }[];
  modes: string[];
}

/** The resolved launch a session carries (the fields this spec reads). */
interface ResolvedLaunch {
  agent: string;
  model: string;
  effort: string;
  fast: string;
  mode: string;
}

async function fetchAgents(weaver: WeaverFixture): Promise<CataloguedAgent[]> {
  const res = await fetch(`${weaver.baseUrl}/api/agents/list`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({}),
  });
  const registry = (await res.json()) as { agents: CataloguedAgent[] };
  return registry.agents;
}

/** Refresh a harness's catalogue through the fake adapter and wait until its
 *  models carry the per-model effort and fast choices the picker needs. */
async function refreshCatalog(weaver: WeaverFixture, harness: string): Promise<CataloguedAgent> {
  const res = await fetch(`${weaver.baseUrl}/api/agents/catalog/refresh`, {
    method: 'POST',
    headers: HEADERS,
    body: JSON.stringify({ harness }),
  });
  const started = (await res.json()) as { started: boolean };
  expect(started.started, 'the refresh must start').toBe(true);

  for (let attempt = 0; attempt < 100; attempt++) {
    const agent = (await fetchAgents(weaver)).find((a) => a.kind === harness);
    const deep = agent?.models.find((m) => m.id === 'fake-deep');
    if (deep && deep.efforts.length > 0 && deep.fast.length > 0) {
      return agent!;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`${harness} catalogue never served fake-deep with its choices`);
}

test.describe('the catalogued model picker', () => {
  test('a catalogued model, effort, fast and mode reach the saved profile', async ({
    page,
    weaver,
  }) => {
    const codex = await refreshCatalog(weaver, 'codex');
    const deep = codex.models.find((m) => m.id === 'fake-deep')!;
    expect(deep.efforts.map((e) => e.id)).toContain('high');

    await page.goto(`${weaver.baseUrl}/settings?tab=agents`);
    await page.getByRole('button', { name: '+ Add profile' }).click();
    const editor = page.getByTestId('profile-editor');
    await editor.getByLabel('Name', { exact: true }).fill('codex-catalog');

    await page.getByTestId('profile-agent').click();
    // Unavailable harnesses list as `<label> — unavailable`; the regex matches
    // both spellings so the spec holds on machines without the CLI.
    await page.getByRole('option', { name: /^Codex( — unavailable)?$/ }).click();

    // The model choices come from the catalogue the probe stored; the combobox
    // commits the id and displays the label.
    await page.getByTestId('profile-model').click();
    await page.getByRole('option', { name: 'Fake deep', exact: true }).click();
    await expect(page.getByTestId('profile-model')).toHaveValue(deep.label);

    // Effort and fast narrow to what the catalogued model declares — the same
    // per-model lists `agents.list` served above.
    await page.getByTestId('profile-effort').click();
    const effortIds = await page
      .getByRole('option')
      .allInnerTexts()
      .then((labels) => labels.filter((label) => deep.efforts.some((e) => e.label === label)));
    expect(effortIds.length, 'the effort options are the model catalogue list').toBe(
      deep.efforts.length,
    );
    await page.getByRole('option', { name: 'High', exact: true }).click();

    await page.getByTestId('profile-fast').click();
    await page.getByRole('option', { name: 'On', exact: true }).click();

    await page.getByTestId('profile-mode').click();
    await page.getByRole('option', { name: 'plan', exact: true }).click();

    await page.getByTestId('profile-save').click();
    await expect(page.getByText('Saved codex-catalog.')).toBeVisible();

    // The saved profile carries every choice the picker offered.
    const res = await fetch(`${weaver.baseUrl}/api/profiles/get`, {
      method: 'POST',
      headers: HEADERS,
      body: JSON.stringify({ name: 'codex-catalog' }),
    });
    const profile = (await res.json()) as {
      agent_kind: string;
      model: string;
      effort: string;
      fast: string;
      mode: string;
      protocol: string;
    };
    expect(profile.agent_kind).toBe('codex');
    expect(profile.model).toBe('fake-deep');
    expect(profile.effort).toBe('high');
    expect(profile.fast).toBe('true');
    expect(profile.mode).toBe('plan');
    expect(profile.protocol).toBe('acp');
  });

  test('launch overrides pick from the catalogue, narrow modes by agent, and reach the launch', async ({
    page,
    weaver,
  }) => {
    const codex = await refreshCatalog(weaver, 'codex');
    expect(codex.modes).toContain('acceptEdits');
    expect(codex.modes).toContain('plan');

    await page.goto(`${weaver.baseUrl}/sessions/new`);
    const settings = page.getByTestId('launch-settings');
    await page.getByPlaceholder('owner/name or /home/you/code/project').fill(weaver.repoPath);
    await page.getByPlaceholder('Add a /health endpoint').fill('say:catalog online');

    // The same catalogued choices, through the launch-override comboboxes.
    await page.getByTestId('override-agent').click();
    await page.getByRole('option', { name: /^Codex( — unavailable)?$/ }).click();
    await page.getByTestId('override-model').click();
    await page.getByRole('option', { name: 'Fake deep', exact: true }).click();

    await settings.locator('summary').click();
    await page.getByTestId('override-effort').click();
    await page.getByRole('option', { name: 'High', exact: true }).click();
    await page.getByTestId('override-fast').click();
    await page.getByRole('option', { name: 'On', exact: true }).click();

    // Codex honours every picker posture, Opencode only auto/default/plan:
    // the mode list must narrow to the agent's own vocabulary, and the
    // model/effort/fast overrides reset rather than leak across agents.
    await page.getByTestId('override-mode').click();
    await expect(page.getByRole('option', { name: 'acceptEdits', exact: true })).toHaveCount(1);
    await page.keyboard.press('Escape');

    await page.getByTestId('override-agent').click();
    await page.getByRole('option', { name: /^Opencode( — unavailable)?$/ }).click();
    await expect(page.getByTestId('override-model')).toHaveValue('');
    await page.getByTestId('override-mode').click();
    await expect(page.getByRole('option', { name: 'acceptEdits', exact: true })).toHaveCount(0);
    await expect(page.getByRole('option', { name: 'bypassPermissions', exact: true })).toHaveCount(
      0,
    );
    await expect(page.getByRole('option', { name: 'plan', exact: true })).toHaveCount(1);
    await page.keyboard.press('Escape');

    // Back to codex, re-pick the catalogued selections, and launch: the
    // session's resolved launch must carry each one.
    await page.getByTestId('override-agent').click();
    await page.getByRole('option', { name: /^Codex( — unavailable)?$/ }).click();
    await page.getByTestId('override-model').click();
    await page.getByRole('option', { name: 'Fake deep', exact: true }).click();
    await page.getByTestId('override-effort').click();
    await expect(page.getByRole('option', { name: 'High', exact: true })).toHaveCount(1);
    await page.getByRole('option', { name: 'High', exact: true }).click();
    await page.getByTestId('override-fast').click();
    await page.getByRole('option', { name: 'On', exact: true }).click();
    await page.getByTestId('override-mode').click();
    await page.getByRole('option', { name: 'plan', exact: true }).click();
    await expect(settings).toContainText('changed');

    await page.getByTestId('create-session').click();
    await expect(page).toHaveURL(/\/s\/[^/]+$/);

    const all = await weaver.listSessions();
    const resolved = (all[0] as unknown as { resolved_launch?: ResolvedLaunch })
      .resolved_launch!;
    expect(resolved.agent).toBe('codex');
    expect(resolved.model).toBe('fake-deep');
    expect(resolved.effort).toBe('high');
    expect(resolved.fast).toBe('true');
    expect(resolved.mode).toBe('plan');
  });
});
