import test from 'node:test';
import assert from 'node:assert/strict';
import { githubLoginHref, loginDestination } from './loginRedirect.ts';

const consent = '/oauth/authorize?client_id=loom-1&state=s';

test('the OAuth consent page is a server destination', () => {
  assert.deepEqual(loginDestination(consent), { kind: 'server', path: consent });
  assert.deepEqual(loginDestination('/s/abc'), { kind: 'spa', path: '/s/abc' });
});

test('foreign or missing destinations fall back to the dashboard', () => {
  for (const redirect of [undefined, '', '//evil.example/x', 'https://evil.example', '/\\evil']) {
    assert.deepEqual(loginDestination(redirect), { kind: 'spa', path: '/' });
  }
});

test('GitHub sign-in carries the destination as next', () => {
  assert.equal(githubLoginHref('/api/auth/github/login', undefined), '/api/auth/github/login');
  assert.equal(
    githubLoginHref('/api/auth/github/login', consent),
    `/api/auth/github/login?next=${encodeURIComponent(consent)}`,
  );
});
