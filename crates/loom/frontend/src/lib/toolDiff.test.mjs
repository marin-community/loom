import test from 'node:test';
import assert from 'node:assert/strict';
import { changedDiffLines } from './toolDiff.ts';

test('whole-file snapshots render only the lines changed by an edit', () => {
  const oldText = ['use axum::Router;', '', 'fn route() {', '    old_handler();', '}', ''].join(
    '\n',
  );
  const newText = ['use axum::Router;', '', 'fn route() {', '    new_handler();', '}', ''].join(
    '\n',
  );

  assert.deepEqual(changedDiffLines(oldText, newText), [
    { sign: '-', text: '    old_handler();' },
    { sign: '+', text: '    new_handler();' },
  ]);
});

test('insertions and deletions remain ordered around unchanged lines', () => {
  assert.deepEqual(
    changedDiffLines('alpha\nremove\nmiddle\nomega\n', 'alpha\nmiddle\nadd\nomega\n'),
    [
      { sign: '-', text: 'remove' },
      { sign: '+', text: 'add' },
    ],
  );
});

test('a missing old snapshot is displayed as a newly added file', () => {
  assert.deepEqual(changedDiffLines(null, 'first\nsecond\n'), [
    { sign: '+', text: 'first' },
    { sign: '+', text: 'second' },
  ]);
});

test('identical snapshots have no changed lines', () => {
  assert.deepEqual(changedDiffLines('same\n', 'same\n'), []);
});

test('a blank line in a new file remains visible', () => {
  assert.deepEqual(changedDiffLines(null, '\n'), [{ sign: '+', text: '' }]);
});
