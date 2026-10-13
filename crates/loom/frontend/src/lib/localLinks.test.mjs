import test from 'node:test';
import assert from 'node:assert/strict';
import { localRoute } from './localLinks.ts';

// The document URL stands in for whatever page loom served: an artifact view.
const DOC = 'http://localhost:5678/s/abc/artifacts/design';

test('absolute same-origin links resolve to their SPA route', () => {
  assert.equal(localRoute('http://localhost:5678/issues', DOC), '/issues');
  assert.equal(
    localRoute('http://localhost:5678/s/x/artifacts/plan?rev=2#top', DOC),
    '/s/x/artifacts/plan?rev=2#top',
  );
});

test('root-relative paths are local routes', () => {
  assert.equal(localRoute('/issues', DOC), '/issues');
  assert.equal(localRoute('/s/abc/changes', DOC), '/s/abc/changes');
  assert.equal(localRoute('/', DOC), '/');
});

test('relative paths resolve against the document URL', () => {
  assert.equal(localRoute('notes', DOC), '/s/abc/artifacts/notes');
  assert.equal(localRoute('./up/../other.md', DOC), '/s/abc/artifacts/other.md');
});

test('other origins, other hosts, and other schemes are not local', () => {
  assert.equal(localRoute('https://example.com/doc', DOC), null);
  assert.equal(localRoute('https://localhost:5678/issues', DOC), null); // scheme differs
  assert.equal(localRoute('http://127.0.0.1:5678/issues', DOC), null); // host differs
  assert.equal(localRoute('http://localhost:9999/issues', DOC), null); // port differs
  assert.equal(localRoute('//example.com/x', DOC), null); // protocol-relative
  assert.equal(localRoute('mailto:person@loom.test', DOC), null);
  assert.equal(localRoute('javascript:alert(1)', DOC), null);
});

test('in-page anchors, empty and unparseable hrefs are not routes', () => {
  assert.equal(localRoute('#heading', DOC), null);
  assert.equal(localRoute('', DOC), null);
  assert.equal(localRoute('http://[broken', DOC), null);
});
