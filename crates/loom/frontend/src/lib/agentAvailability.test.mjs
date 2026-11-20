import test from 'node:test';
import assert from 'node:assert/strict';
import {
  agentOptionsWithCurrent,
  availableAgentKinds,
  availableAgents,
  isAgentAvailable,
  profileAgentAvailable,
} from './agentAvailability.ts';

const agent = (kind, available) => ({ kind, label: kind, available });

test('only an explicit false marks an agent unavailable', () => {
  assert.equal(isAgentAvailable(agent('claude', undefined)), true);
  assert.equal(isAgentAvailable(agent('claude', null)), true);
  assert.equal(isAgentAvailable(agent('claude', true)), true);
  assert.equal(isAgentAvailable(agent('codex', false)), false);
});

test('availableAgents / availableAgentKinds drop the unavailable', () => {
  const agents = [agent('claude', true), agent('codex', false), agent('custom', undefined)];
  assert.deepEqual(
    availableAgents(agents).map((a) => a.kind),
    ['claude', 'custom'],
  );
  assert.deepEqual([...availableAgentKinds(agents)], ['claude', 'custom']);
});

test('profileAgentAvailable checks membership in the installed kinds', () => {
  const kinds = new Set(['claude']);
  assert.equal(profileAgentAvailable({ agent_kind: 'claude' }, kinds), true);
  assert.equal(profileAgentAvailable({ agent_kind: 'codex' }, kinds), false);
});

test('agentOptionsWithCurrent keeps the current selection visible', () => {
  const agents = [agent('claude', true), agent('codex', false)];

  // Current agent is available — list is just the available ones.
  assert.deepEqual(
    agentOptionsWithCurrent(agents, 'claude').map((a) => a.kind),
    ['claude'],
  );

  // Current agent is unavailable — it is appended, still flagged unavailable.
  const withCodex = agentOptionsWithCurrent(agents, 'codex');
  assert.deepEqual(
    withCodex.map((a) => a.kind),
    ['claude', 'codex'],
  );
  assert.equal(isAgentAvailable(withCodex[1]), false);

  // Current agent unknown to the server — a synthetic, unavailable entry.
  const withGhost = agentOptionsWithCurrent(agents, 'ghost');
  assert.deepEqual(
    withGhost.map((a) => a.kind),
    ['claude', 'ghost'],
  );
  assert.equal(isAgentAvailable(withGhost[1]), false);

  // No current selection — untouched available list.
  assert.deepEqual(
    agentOptionsWithCurrent(agents, null).map((a) => a.kind),
    ['claude'],
  );
});
