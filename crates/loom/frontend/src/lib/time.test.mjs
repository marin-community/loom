import test from 'node:test';
import assert from 'node:assert/strict';
import { localTime } from './time.ts';

test('clock times are converted from UTC to the viewer timezone', () => {
  assert.equal(
    localTime('2026-09-02T15:02:00Z', { timeZone: 'America/Los_Angeles' }, 'en-US'),
    '8:02 AM',
  );
});

test('clock times follow the viewer locale and can include seconds', () => {
  assert.equal(
    localTime(
      '2026-09-02T15:02:09Z',
      { timeZone: 'America/Los_Angeles', second: '2-digit' },
      'en-GB',
    ),
    '8:02:09',
  );
});

test('missing and invalid clock times remain safe to render', () => {
  assert.equal(localTime(null), '');
  assert.equal(localTime('not-a-time'), 'not-a-time');
});
