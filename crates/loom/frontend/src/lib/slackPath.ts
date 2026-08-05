import type { SlackStatus } from '../types';

/** How a link in the trigger path is doing.
 *
 *  `ok` carries the phosphor green the visual system reserves for healthy
 *  state, `attn` the amber it reserves for "wants a look", and `off` the faint
 *  grey of a link that is deliberately not in use. `wait` is the pre-answer
 *  state, held separately so a slow request never reads as a fault. */
export type LinkState = 'ok' | 'attn' | 'off' | 'wait';

/** One step a Slack message takes on its way to becoming a session. */
export interface PathLink {
  key: string;
  /** What this link does, in the operator's words. */
  label: string;
  state: LinkState;
  /** The machine fact behind the state — an id, a count, a mode. Rendered mono. */
  detail: string;
  /** What to do about it, present only when this link is what's broken. */
  fix?: string;
}

const OK_MARK = 'ok';

/** The Slack trigger path, in the order the server actually checks it.
 *
 *  A message becomes a session only if every link holds, which is why this is
 *  a sequence and not a status flag: an integration can hold a live socket and
 *  still discard every mention. Rendering the whole path means the broken link
 *  names itself instead of hiding behind "connected". */
export function slackPath(s: SlackStatus): PathLink[] {
  const socket = s.socket;
  const identity = s.identity;

  const tokens: PathLink = {
    key: 'tokens',
    label: 'Tokens',
    state: s.configured ? OK_MARK : 'off',
    detail: s.configured
      ? 'app-level and bot tokens set'
      : [
          s.app_token_set ? null : 'LOOM_SLACK_APP_TOKEN',
          s.bot_token_set ? null : 'LOOM_SLACK_BOT_TOKEN',
        ]
          .filter(Boolean)
          .join(' and ') + ' not set',
    fix: s.configured ? undefined : 'Set both in the server environment, then restart loom.',
  };

  const enabled: PathLink = {
    key: 'enabled',
    label: 'Switch',
    state: s.enabled ? OK_MARK : 'off',
    detail: s.enabled ? 'on' : 'off',
    fix: s.enabled ? undefined : 'Turn Slack integration on below.',
  };

  const socketLabels: Record<string, { state: LinkState; detail: string }> = {
    connected: { state: OK_MARK, detail: 'listening' },
    connecting: { state: 'wait', detail: 'opening the socket' },
    failed: { state: 'attn', detail: socket.last_error ?? 'connection failed' },
    idle: { state: 'off', detail: 'not connected' },
  };
  const socketRow = socketLabels[socket.state] ?? socketLabels.idle;
  const connection: PathLink = {
    key: 'socket',
    label: 'Connection',
    state: socketRow.state,
    detail: socket.app_id ? `${socketRow.detail} · app ${socket.app_id}` : socketRow.detail,
    fix:
      socket.state === 'failed'
        ? 'Check the app-level token, then watch the server log for the retry.'
        : undefined,
  };

  const identityRow = ((): PathLink => {
    if (!identity) {
      return { key: 'identity', label: 'Identity', state: 'off', detail: 'no bot token' };
    }
    if (identity.error) {
      return {
        key: 'identity',
        label: 'Identity',
        state: 'attn',
        detail: identity.error,
        fix: 'Slack rejected the bot token. Reinstall the app and set the new token.',
      };
    }
    // A person's token authenticates and connects exactly like the app's, so
    // this is the one link that looks healthy while the integration is dead.
    if (identity.token_kind === 'user') {
      return {
        key: 'identity',
        label: 'Identity',
        state: 'attn',
        detail: `${identity.user_id} · a person, not the app`,
        fix:
          'LOOM_SLACK_BOT_TOKEN holds a user token (xoxp-…). loom posts as that ' +
          'person and drops their mentions as its own. Use the app’s bot token (xoxb-…).',
      };
    }
    return {
      key: 'identity',
      label: 'Identity',
      state: OK_MARK,
      detail: `${identity.user_id} in ${identity.team_id}`,
    };
  })();

  const access: PathLink =
    s.access.mode === 'workspace'
      ? {
          key: 'access',
          label: 'Who can trigger',
          state: OK_MARK,
          detail: 'anyone in the workspace, where the bot is invited',
        }
      : {
          key: 'access',
          label: 'Who can trigger',
          state: OK_MARK,
          detail: `${s.access.users.length} listed ${s.access.users.length === 1 ? 'person' : 'people'}`,
        };

  const repo: PathLink = {
    key: 'repo',
    label: 'Repository',
    state: s.default_repo ? OK_MARK : 'attn',
    detail: s.default_repo || 'none',
    fix: s.default_repo
      ? undefined
      : 'Set a default repository below, or prefix each request with owner/name:.',
  };

  return [tokens, enabled, connection, identityRow, access, repo];
}

/** The first link standing between a Slack message and a session, or `null`
 *  when the whole path holds. Drives the pane's headline, so the operator reads
 *  the diagnosis before the detail. */
export function firstBreak(links: PathLink[]): PathLink | null {
  return links.find((l) => l.state === 'attn' || l.state === 'off') ?? null;
}

/** The pane's one-line verdict. */
export function pathVerdict(links: PathLink[]): { text: string; tone: LinkState } {
  if (links.some((l) => l.state === 'wait')) {
    return { text: 'Connecting to Slack…', tone: 'wait' };
  }
  const broken = firstBreak(links);
  if (!broken) return { text: 'Ready for @loom and /loom', tone: OK_MARK };
  if (broken.state === 'off' && broken.key === 'tokens') {
    return { text: 'Slack is not set up', tone: 'off' };
  }
  if (broken.state === 'off' && broken.key === 'enabled') {
    return { text: 'Slack is switched off', tone: 'off' };
  }
  return { text: `Mentions stop at ${broken.label.toLowerCase()}`, tone: broken.state };
}
