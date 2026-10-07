import test from 'node:test';
import assert from 'node:assert/strict';
import { sessionTitleParts } from './sessionTitle.ts';

test('a top-level title keeps a slash in the foreground', () => {
  assert.deepEqual(sessionTitleParts('Save sessions filter / sort'), [
    { text: 'Save sessions filter / sort', muted: false },
  ]);
});

test('a nested row mutes only the prefix shared with its parent', () => {
  assert.deepEqual(sessionTitleParts('Project / Child', { parentTitle: 'Project / Parent' }), [
    { text: 'Project', muted: true },
    { text: 'Child', muted: false },
  ]);
  assert.deepEqual(sessionTitleParts('Area / Task / Step', { parentTitle: 'Area / Task' }), [
    { text: 'Area', muted: true },
    { text: 'Task', muted: true },
    { text: 'Step', muted: false },
  ]);
});

test('a slash that does not continue the parent path stays in the title', () => {
  assert.deepEqual(
    sessionTitleParts('Save sessions filter / sort', { parentTitle: 'Project / Parent' }),
    [{ text: 'Save sessions filter / sort', muted: false }],
  );
});

test('smart views mute the group and leave the task title intact', () => {
  assert.deepEqual(sessionTitleParts('Save sessions filter / sort', { group: 'Inbox' }), [
    { text: 'Inbox', muted: true },
    { text: 'Save sessions filter / sort', muted: false },
  ]);
});

test('ancestor matching ignores case and skips a blank group', () => {
  assert.deepEqual(sessionTitleParts('Area / Task', { group: '  ', parentTitle: 'area' }), [
    { text: 'Area', muted: true },
    { text: 'Task', muted: false },
  ]);
});
