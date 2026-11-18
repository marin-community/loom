import test from 'node:test';
import assert from 'node:assert/strict';
import {
  MODE_CHOICES,
  clearedForAgentChange,
  effortChoices,
  fastChoices,
  modelById,
  protocolChoices,
  prunedForModelChange,
} from './pickerChoices.ts';

const choice = (id) => ({ id, label: id, efforts: [], fast: [] });
const agent = (kind, extra = {}) => ({
  kind,
  label: kind,
  models: [],
  efforts: [],
  accepts_raw_model: false,
  supports_hooks: false,
  builtin: true,
  supports_acp: true,
  protocol: 'acp',
  available: true,
  ...extra,
});

test('a model that scopes effort owns the list, otherwise the agent list applies', () => {
  const scaled = agent('claude', {
    efforts: [choice('low'), choice('high')],
  });
  const scoping = { ...choice('opus'), efforts: [choice('medium')] };

  assert.deepEqual(
    effortChoices(scaled, scoping).map((c) => c.id),
    ['medium'],
  );
  // Unfetched model: fall back to the agent's global list.
  assert.deepEqual(
    effortChoices(scaled, choice('opus')).map((c) => c.id),
    ['low', 'high'],
  );
  // No agent at all: an empty list, so the picker offers only "Agent default".
  assert.deepEqual(effortChoices(undefined, undefined), []);
});

test('an empty per-model effort list never hides the agent list', () => {
  // The wire omits these keys on older servers; `?.` keeps the fallback intact
  // rather than throwing on undefined.
  const sparse = agent('codex', { efforts: [choice('low')] });
  const bare = { id: 'gpt-5.5', label: 'GPT-5.5' };
  assert.deepEqual(
    effortChoices(sparse, bare).map((c) => c.id),
    ['low'],
  );
  assert.deepEqual(fastChoices(bare), []);
});

test('fast is per model only', () => {
  assert.deepEqual(fastChoices(undefined), []);
  const model = { ...choice('composer'), fast: [choice('true'), choice('false')] };
  assert.deepEqual(
    fastChoices(model).map((c) => c.id),
    ['true', 'false'],
  );
});

test('only an ACP-capable harness offers the ACP protocol', () => {
  assert.deepEqual(
    protocolChoices(agent('claude')).map((c) => c.id),
    ['acp', 'terminal'],
  );
  assert.deepEqual(
    protocolChoices(agent('plain', { supports_acp: false })).map((c) => c.id),
    ['terminal'],
  );
  assert.deepEqual(
    protocolChoices(undefined).map((c) => c.id),
    ['acp', 'terminal'],
  );
});

test('every mode the harness accepts is offered', () => {
  assert.deepEqual(
    MODE_CHOICES.map((c) => c.id),
    ['auto', 'default', 'acceptEdits', 'plan', 'bypassPermissions'],
  );
});

test('changing the agent clears every selector resolved for the old one', () => {
  assert.deepEqual(clearedForAgentChange(), { model: '', effort: '', fast: '' });
});

test('changing model prunes only selectors the new model states', () => {
  // The new model lists efforts and the current one is absent -> cleared.
  const scoping = { ...choice('opus'), efforts: [choice('high')] };
  assert.deepEqual(prunedForModelChange(scoping, { effort: 'low', fast: '' }), {
    effort: '',
  });

  // Current effort is still offered -> kept.
  assert.deepEqual(prunedForModelChange(scoping, { effort: 'high', fast: '' }), {});

  // An unfetched model states nothing, so a value it might accept is kept.
  assert.deepEqual(prunedForModelChange(choice('sonnet'), { effort: 'high', fast: 'true' }), {});
  assert.deepEqual(prunedForModelChange(undefined, { effort: 'high', fast: 'true' }), {});
});

test('changing model prunes fast against the same rule', () => {
  const fastModel = { ...choice('composer'), fast: [choice('false')] };
  assert.deepEqual(prunedForModelChange(fastModel, { effort: '', fast: 'true' }), { fast: '' });
  assert.deepEqual(prunedForModelChange(fastModel, { effort: '', fast: 'false' }), {});
});

test('modelById finds the model on the selected agent only', () => {
  const model = choice('opus');
  const claude = agent('claude', { models: [model] });
  assert.equal(modelById(claude, 'opus'), model);
  assert.equal(modelById(claude, 'missing'), undefined);
  assert.equal(modelById(undefined, 'opus'), undefined);
});
