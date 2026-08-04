import test from 'node:test';
import assert from 'node:assert/strict';
import { githubCompactChip } from './githubStatus.ts';
import { compactAge } from './time.ts';

const status = (overrides = {}) => ({
  pr_state: 'OPEN',
  is_draft: false,
  checks: null,
  ...overrides,
});

test('compact PR status uses explicit CI words', () => {
  assert.equal(githubCompactChip(status({ checks: 'passing' })).label, 'OK');
  assert.equal(githubCompactChip(status({ checks: 'pending' })).label, 'TESTING');
  assert.equal(githubCompactChip(status({ checks: 'failing' })).label, 'FAILED');
  assert.equal(githubCompactChip(status()).label, 'PENDING');
});

test('terminal and draft PR states override CI', () => {
  assert.equal(githubCompactChip(status({ is_draft: true, checks: 'passing' })).label, 'DRAFT');
  assert.equal(
    githubCompactChip(status({ pr_state: 'MERGED', checks: 'passing' })).label,
    'MERGED',
  );
  assert.equal(
    githubCompactChip(status({ pr_state: 'CLOSED', checks: 'passing' })).label,
    'CLOSED',
  );
});

test('compact age drops prose without losing its unit', () => {
  const now = Date.parse('2026-08-04T12:00:00Z');
  assert.equal(compactAge('2026-08-04T11:59:50Z', now), 'now');
  assert.equal(compactAge('2026-08-04T11:50:00Z', now), '10m');
  assert.equal(compactAge('2026-08-04T11:00:00Z', now), '1h');
});
