import test from 'node:test';
import assert from 'node:assert/strict';
import { slackPath, firstBreak, pathVerdict } from './slackPath.ts';

const status = (overrides = {}) => ({
  enabled: true,
  app_token_set: true,
  bot_token_set: true,
  configured: true,
  identity: { user_id: 'U_BOT', team_id: 'T1', token_kind: 'bot', error: null },
  access: { mode: 'workspace', users: [] },
  default_repo: 'acme/web',
  socket: {
    state: 'connected',
    app_id: 'A1',
    connected_at: '2026-08-05T01:00:00Z',
    last_error: null,
    last_event_at: null,
    events_received: 0,
    sessions_launched: 0,
    last_skip: null,
    last_skip_at: null,
  },
  ...overrides,
});

const link = (s, key) => slackPath(s).find((l) => l.key === key);

test('a whole path reports ready, with no link singled out', () => {
  const links = slackPath(status());
  assert.equal(firstBreak(links), null);
  assert.equal(pathVerdict(links).text, 'Ready for @loom and /loom');
  assert.ok(links.every((l) => l.state === 'ok'));
});

test('a user token is a fault, not a healthy identity', () => {
  // The production failure this pane exists for: auth.test succeeds, the socket
  // is live, and every mention the token's owner types is dropped as loom's own.
  const s = status({
    identity: { user_id: 'U_HUMAN', team_id: 'T1', token_kind: 'user', error: null },
  });
  const identity = link(s, 'identity');
  assert.equal(identity.state, 'attn');
  assert.match(identity.fix, /xoxb/);
  assert.equal(pathVerdict(slackPath(s)).text, 'Mentions stop at identity');
});

test('a live socket does not mask a later broken link', () => {
  const s = status({ default_repo: '' });
  assert.equal(link(s, 'socket').state, 'ok');
  assert.equal(firstBreak(slackPath(s)).key, 'repo');
  assert.equal(pathVerdict(slackPath(s)).text, 'Mentions stop at repository');
});

test('the earliest break wins, so the operator fixes causes before symptoms', () => {
  const s = status({
    enabled: false,
    socket: { ...status().socket, state: 'idle' },
    default_repo: '',
  });
  assert.equal(firstBreak(slackPath(s)).key, 'enabled');
  assert.equal(pathVerdict(slackPath(s)).text, 'Slack is switched off');
});

test('missing tokens name the variables that are missing', () => {
  const s = status({ app_token_set: false, bot_token_set: false, configured: false });
  assert.match(link(s, 'tokens').detail, /LOOM_SLACK_APP_TOKEN and LOOM_SLACK_BOT_TOKEN/);
  assert.equal(pathVerdict(slackPath(s)).text, 'Slack is not set up');
});

test('both access modes are healthy, and each says who it admits', () => {
  assert.match(link(status(), 'access').detail, /anyone in the workspace/);
  const listed = status({ access: { mode: 'listed', users: ['U1', 'U2'] } });
  assert.equal(link(listed, 'access').detail, '2 listed people');
  assert.equal(firstBreak(slackPath(listed)), null);
});

test('connecting reads as pending, never as a fault', () => {
  const s = status({ socket: { ...status().socket, state: 'connecting', app_id: null } });
  assert.equal(link(s, 'socket').state, 'wait');
  assert.equal(pathVerdict(slackPath(s)).text, 'Connecting to Slack…');
});

test('a failed socket surfaces the error Slack gave', () => {
  const s = status({
    socket: { ...status().socket, state: 'failed', last_error: 'invalid_auth' },
  });
  assert.equal(link(s, 'socket').detail, 'invalid_auth · app A1');
  assert.equal(link(s, 'socket').state, 'attn');
});
