import test from 'node:test';
import assert from 'node:assert/strict';
import { classifyImageSource, resolvePath } from './markdownImages.ts';

// The classifier decides where an inline image source points; the URL
// resolver in `markdown.ts` turns each kind into an endpoint. These tests pin
// the mapping the conversation view depends on: agent-authored host paths must
// become `host` sources, never pass-through web paths (that is how inline
// images used to render as the browser's broken-image icon).

test('artifact references need a storable name', () => {
  assert.deepEqual(classifyImageSource('', 'artifact:mockup.png'), {
    kind: 'artifact',
    name: 'mockup.png',
  });
  // An unnameable artifact is still a scheme-ful URL: it passes through, like
  // any other non-http scheme the renderer does not understand.
  assert.equal(classifyImageSource('', 'artifact:has space.png').kind, 'external');
  assert.equal(classifyImageSource('', 'artifact:../escape').kind, 'external');
});

test('host-absolute paths become host sources', () => {
  const cases = [
    ['/tmp/opencode/option-a.png', '/tmp/opencode/option-a.png'],
    ['/tmp/opencode/shot.png?raw=1#frag', '/tmp/opencode/shot.png'],
    ['/tmp/opencode/my%20shot.png', '/tmp/opencode/my shot.png'],
    ['file:///tmp/opencode/shot.png', '/tmp/opencode/shot.png'],
    ['file://localhost/tmp/shot.png', '/tmp/shot.png'],
  ];
  for (const [url, path] of cases) {
    assert.deepEqual(classifyImageSource('reports', url), { kind: 'host', path }, url);
  }
});

test('a non-local file URL stays a passthrough', () => {
  assert.deepEqual(classifyImageSource('', 'file://server/share/shot.png'), {
    kind: 'external',
    url: 'file://server/share/shot.png',
  });
});

test('loom API paths and web URLs pass through untouched', () => {
  const passthrough = [
    '/api/artifacts/raw?branch=s&name=shot.png',
    '/api/sessions/raw?session=s&path=shot.png',
    'https://example.com/shot.png',
    '//cdn.example.com/shot.png',
    'data:image/png;base64,iVBOR',
    '#fragment',
    '',
  ];
  for (const url of passthrough) {
    assert.deepEqual(classifyImageSource('', url), { kind: 'external', url }, url);
  }
});

test('relative sources resolve against the document directory', () => {
  assert.deepEqual(classifyImageSource('reports', 'shot.png'), {
    kind: 'worktree',
    path: 'reports/shot.png',
  });
  assert.deepEqual(classifyImageSource('reports/deep', '../up.png'), {
    kind: 'worktree',
    path: 'reports/up.png',
  });
  assert.deepEqual(classifyImageSource('', 'shot.png'), {
    kind: 'worktree',
    path: 'shot.png',
  });
  // Undecodable percent-encoding is left alone rather than guessed at.
  assert.equal(classifyImageSource('', 'bad%zz.png').kind, 'external');
});

test('resolvePath collapses dot segments', () => {
  assert.equal(resolvePath('a/b', 'c/../d/./e.png'), 'a/b/d/e.png');
  assert.equal(resolvePath('a/b', '../../../../top.png'), 'top.png');
  assert.equal(resolvePath('', './x.png'), 'x.png');
});
