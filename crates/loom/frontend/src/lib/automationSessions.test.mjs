import test from 'node:test';
import assert from 'node:assert/strict';
import { unmatchedRunTitle } from './automationRunLabels.ts';

test('waiting launch reservations are presented as blocked', () => {
  assert.equal(unmatchedRunTitle({ status: 'waiting' }, 'intervention'), 'Launch blocked');
});

test('other unmatched run projections retain their lifecycle labels', () => {
  assert.equal(unmatchedRunTitle({ status: 'failed' }, 'intervention'), 'Launch failed');
  assert.equal(
    unmatchedRunTitle({ status: 'creating' }, 'provisioning'),
    'Provisioning · creating',
  );
  assert.equal(unmatchedRunTitle({ status: 'completed' }, 'history'), 'Run completed');
});
