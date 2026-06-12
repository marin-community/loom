import { test as base, expect } from '@playwright/test';
import { type ChildProcess, execFileSync, spawn } from 'child_process';
import { randomBytes } from 'crypto';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';

// Repo layout: this file lives at <weaver>/e2e/fixtures/weaver.ts
const WEAVER_ROOT = join(__dirname, '..', '..');
const LOOM_BINARY = join(WEAVER_ROOT, 'target', 'debug', 'loom');
const WEAVER_BINARY = join(WEAVER_ROOT, 'target', 'debug', 'weaver');
const FRONTEND_DIR = join(WEAVER_ROOT, 'crates', 'loom', 'frontend');
const DIST_INDEX = join(WEAVER_ROOT, 'crates', 'loom', 'static', 'dist', 'index.html');

/** One (key, value) annotation on a branch. The well-known loud keys are
 *  `attention` (the agent) and `triage` (an overlooker / `manual`); any other
 *  key is a quiet pill. Absence is the calm state — there is no `ok` tag. */
export interface TagView {
  key: string;
  value: string;
  note: string;
  set_by: string;
  set_at: string;
}

/** The branch-level fields embedded in a SessionView. */
export interface Branch {
  id: string;
  name: string;
  title: string;
  goal: string;
  /** Current-state message, set with the `attention` tag via `weaver set-status`. */
  description: string;
  /** Every tag on the branch (the agent's `attention`, an overlooker's
   *  `triage`, any free-form key). Empty when calm — absence is the default. */
  tags: TagView[];
  repo_root: string;
  branch: string;
  base_branch: string;
  created_at: string;
  updated_at: string;
  open_issue_count: number;
}

/** A session as returned by `/api/sessions[/...]`. */
export interface Session {
  id: string;
  status: string;
  work_dir: string;
  tmux_session: string;
  agent_kind: string;
  pending_prompt: string;
  github_repo: string | null;
  last_activity_at: string;
  created_at: string;
  updated_at: string;
  /** Branch id of the session that launched this one, or null at the top level. */
  parent_id: string | null;
  branch: Branch;
}

export interface SeedOpts {
  goal: string;
  /** Title; defaults to `name` so the detail heading is predictable in tests. */
  title?: string;
  name?: string;
  base?: string;
  /** Branch id of the launching session — sets this session's tree parent. */
  parent?: string;
}

/** An overlooker as returned by `/api/overlookers` (the fields the e2e tests
 *  read; the full DTO has more). */
export interface Overlooker {
  id: string;
  name: string;
  enabled: boolean;
  program: string;
  capabilities: string[];
  last_outcome: string | null;
}

/** An issue as returned by `/api/issues` (the fields the e2e tests read). */
export interface Issue {
  id: number;
  repo_root: string;
  source_branch: string | null;
  claimed_branch: string | null;
  title: string;
  body: string;
  status: string;
  tags: TagView[];
}

export interface SeedOverlookerOpts {
  name: string;
  /** Trigger predicate; defaults to a manual `{}` (only fires on Run now). */
  trigger?: Record<string, unknown>;
  /** Fleet scope; defaults to `{}` (whole fleet). */
  scope?: Record<string, unknown>;
  program?: string;
  params?: Record<string, unknown>;
  capabilities?: string[];
}

export interface WeaverFixture {
  /** Base URL of the running loom server, e.g. http://127.0.0.1:NNNN */
  baseUrl: string;
  /** Path to the throwaway git repo (one commit on `main`) used as `cwd`. */
  repoPath: string;
  /** Create a session directly via the API using the `shell` agent. */
  seedSession(opts: SeedOpts): Promise<Session>;
  /** Register an overlooker directly via the API. */
  seedOverlooker(opts: SeedOverlookerOpts): Promise<Overlooker>;
  /** Create an issue claimed by a seeded session's branch (so it shares the
   *  session's canonical repo_root and resolves back to it in the Issues pane). */
  seedIssue(session: Session, title: string, body?: string): Promise<Issue>;
  /** Set (upsert) a free-form label on an issue via `PUT …/issues/{id}/tags/{key}`. */
  tagIssue(id: number, key: string, value: string): Promise<Issue>;
  /** GET /api/issues (cross-repo board). */
  listIssues(all?: boolean): Promise<Issue[]>;
  /** GET /api/sessions/{id}. */
  getSession(id: string): Promise<Session>;
  /** GET /api/sessions. */
  listSessions(): Promise<Session[]>;
  /** Flip a session's status by writing a hook event row via `weaver hook`. */
  hook(session: Session, event: 'working' | 'waiting' | 'idle'): Promise<void>;
  /** Declare the agent's status (level + message) via `weaver set-status`. It
   *  writes the branch's `attention` tag (clearing it on `ok`) and the
   *  current-state message, recording a `tag` event the monitor re-broadcasts. */
  setStatus(
    session: Session,
    level: 'ok' | 'attention' | 'blocked',
    message?: string,
  ): Promise<void>;
  /** Set (upsert) one tag on a session's branch via `PUT …/tags/{key}`. */
  setTag(
    session: Session,
    key: string,
    value: string,
    opts?: { note?: string; by?: string },
  ): Promise<void>;
  /** Clear one tag via `DELETE …/tags/{key}`. */
  clearTag(session: Session, key: string): Promise<void>;
  /** Stamp an overlooker's `triage` mark — sugar over `setTag(triage, …)`. */
  mark(
    session: Session,
    level: 'attention' | 'blocked',
    opts?: { note?: string; by?: string },
  ): Promise<void>;
}

/**
 * Ensure the loom/weaver binaries and the Vue SPA bundle all exist. Called once
 * from `globalSetup` (see playwright.config.ts) — before any worker spawns — so
 * parallel workers never race on a concurrent `cargo build` / rspack write.
 */
export function ensureBuilt() {
  // Always run an incremental `cargo build` (it builds both binaries and the SPA
  // into static/dist via build.rs). `rerun-if-changed` makes it a fast no-op when
  // nothing changed, but it rebuilds a stale bundle after a backend *or* frontend
  // edit — so the suite never tests an out-of-date or placeholder UI.
  execFileSync('cargo', ['build'], {
    cwd: WEAVER_ROOT,
    stdio: 'inherit',
    env: process.env,
  });
  if (!existsSync(LOOM_BINARY)) {
    throw new Error(`loom binary missing after build: ${LOOM_BINARY}`);
  }
  if (!existsSync(WEAVER_BINARY)) {
    throw new Error(`weaver binary missing after build: ${WEAVER_BINARY}`);
  }
  if (!existsSync(DIST_INDEX)) {
    // build.rs writes a placeholder when Node is unavailable; build the SPA
    // directly so the UI under test is the real one.
    execFileSync('npx', ['rspack', 'build'], {
      cwd: FRONTEND_DIR,
      stdio: 'inherit',
      env: process.env,
    });
  }
}

/** Create a throwaway git repo with a single commit on `main`. */
function makeRepo(dir: string) {
  mkdirSync(dir, { recursive: true });
  const git = (args: string[]) =>
    execFileSync('git', args, { cwd: dir, stdio: 'pipe' });
  git(['init', '-b', 'main']);
  git(['config', 'user.name', 'Weaver E2E']);
  git(['config', 'user.email', 'e2e@weaver.test']);
  writeFileSync(join(dir, 'README.md'), '# weaver e2e fixture repo\n');
  git(['add', '-A']);
  git(['commit', '-m', 'initial commit']);
}

async function fetchJson(url: string, init?: RequestInit): Promise<unknown> {
  const res = await fetch(url, {
    headers: { 'content-type': 'application/json' },
    ...init,
  });
  if (!res.ok) {
    throw new Error(`${init?.method ?? 'GET'} ${url} → ${res.status}: ${await res.text()}`);
  }
  const text = await res.text();
  return text ? JSON.parse(text) : null;
}

/** Delete every session on a server (and its branch/worktree), best-effort. */
async function deleteAllSessions(baseUrl: string) {
  try {
    const all = (await fetchJson(`${baseUrl}/api/sessions`)) as Session[];
    for (const s of all) {
      try {
        await fetch(`${baseUrl}/api/sessions/${s.id}?keep_branch=false`, {
          method: 'DELETE',
        });
      } catch {
        /* best effort */
      }
    }
  } catch {
    /* server may already be gone */
  }
}

/** Delete every overlooker on a server, best-effort — overlookers aren't tied
 *  to a session, so the per-test wipe clears them explicitly. */
async function deleteAllOverlookers(baseUrl: string) {
  try {
    const all = (await fetchJson(`${baseUrl}/api/overlookers`)) as { id: string }[];
    for (const o of all) {
      try {
        await fetch(`${baseUrl}/api/overlookers/${o.id}`, { method: 'DELETE' });
      } catch {
        /* best effort */
      }
    }
  } catch {
    /* server may already be gone */
  }
}

/** Delete every issue on a server, best-effort. Issues are repo-owned and
 *  survive session teardown (claims are released to the backlog), and a launch
 *  opens a tracking issue — so the per-test wipe clears them explicitly to keep
 *  count-based assertions ("0 issues") order-independent. */
async function deleteAllIssues(baseUrl: string) {
  try {
    const all = (await fetchJson(`${baseUrl}/api/issues?all=true`)) as { id: number }[];
    for (const i of all) {
      try {
        await fetch(`${baseUrl}/api/issues/${i.id}`, { method: 'DELETE' });
      } catch {
        /* best effort */
      }
    }
  } catch {
    /* server may already be gone */
  }
}

/** A loom server shared by every test in one Playwright worker. */
interface ServerHandle {
  baseUrl: string;
  repoPath: string;
  /** Env for spawning `weaver` against this server (WEAVER_HOME/DB/TMUX_SOCKET). */
  childEnv: NodeJS.ProcessEnv;
}

interface WorkerFixtures {
  server: ServerHandle;
}

export const test = base.extend<{ weaver: WeaverFixture }, WorkerFixtures>({
  // One loom server per worker, reused across all of that worker's tests. Booting
  // a server (build a throwaway repo, spawn `loom serve`, start a private tmux
  // server) is the expensive part; the per-test `weaver` fixture below just wipes
  // sessions between tests so each starts from a clean slate. Workers are fully
  // isolated (own WEAVER_HOME/db, port, and tmux socket), so they run in parallel
  // safely — see playwright.config.ts.
  server: [
    async ({}, use, workerInfo) => {
      const tmpDir = mkdtempSync(join(tmpdir(), 'weaver-e2e-'));
      const weaverHome = join(tmpDir, 'home');
      const dbPath = join(tmpDir, 'db.sqlite');
      const repoPath = join(tmpDir, 'repo');
      mkdirSync(weaverHome, { recursive: true });
      makeRepo(repoPath);

      // Pin tmux to a private throwaway server (`tmux -L <name>`), exactly like
      // the Rust integration harness, so a worker's sessions never land on — or
      // get torn down from — the machine-global default socket where the user's
      // real weaver-<id> agents (including the one running you) live.
      // `socket_args()` prepends `-L <name>` to every loom tmux call (create /
      // kill / capture / attach), so this one var namespaces the whole worker.
      // The name is unique per worker; reap any stale server from a crashed run.
      const tmuxSocket = `weaver-e2e-${process.pid}-w${workerInfo.workerIndex}-${randomBytes(3).toString('hex')}`;
      try {
        execFileSync('tmux', ['-L', tmuxSocket, 'kill-server'], { stdio: 'ignore' });
      } catch {
        /* no such server yet — fine */
      }

      // Per-worker env: every spawned process (loom + weaver hooks) sees the same
      // WEAVER_HOME / WEAVER_DB so they share one database, and the same
      // WEAVER_TMUX_SOCKET so all tmux ops stay on the private server above.
      const childEnv = {
        ...process.env,
        WEAVER_HOME: weaverHome,
        WEAVER_DB: dbPath,
        WEAVER_TMUX_SOCKET: tmuxSocket,
        RUST_LOG: 'loom=warn,weaver_core=warn',
      };

      // Bind to a random free port (0) and parse the actual port from stdout.
      const server: ChildProcess = spawn(LOOM_BINARY, ['serve', '--addr', '127.0.0.1:0'], {
        env: childEnv,
        stdio: ['ignore', 'pipe', 'pipe'],
      });

      let baseUrl = '';
      let serverLog = '';
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(
          () => reject(new Error(`loom did not start in 20s. Output:\n${serverLog}`)),
          20_000,
        );
        const onData = (chunk: Buffer) => {
          serverLog += chunk.toString();
          const m = serverLog.match(/listening on (http:\/\/[\d.]+:\d+)/);
          if (m && !baseUrl) {
            baseUrl = m[1];
            clearTimeout(timer);
            resolve();
          }
        };
        server.stdout!.on('data', onData);
        server.stderr!.on('data', onData);
        server.on('error', (err) => {
          clearTimeout(timer);
          reject(err);
        });
        server.on('exit', (code) => {
          if (!baseUrl) {
            clearTimeout(timer);
            reject(new Error(`loom exited with code ${code} before listening:\n${serverLog}`));
          }
        });
      });

      // Wait for the API to actually answer.
      let healthy = false;
      for (let i = 0; i < 50; i++) {
        try {
          const res = await fetch(`${baseUrl}/api/health`);
          if (res.ok) {
            healthy = true;
            break;
          }
        } catch {
          /* not ready */
        }
        await new Promise((r) => setTimeout(r, 100));
      }
      if (!healthy) throw new Error(`loom /api/health never returned ok:\n${serverLog}`);

      // UI-created sessions should use a plain shell, never the real claude CLI.
      await fetchJson(`${baseUrl}/api/settings`, {
        method: 'PATCH',
        body: JSON.stringify({ 'agent.default': 'shell' }),
      });

      await use({ baseUrl, repoPath, childEnv });

      // --- Worker teardown: stop the server, reap the private tmux server, and
      // remove temp dirs. Everything here is scoped to this worker's private
      // socket and db, so the user's real sessions are never touched.
      await deleteAllSessions(baseUrl);
      await new Promise<void>((resolve) => {
        let done = false;
        const finish = () => {
          if (!done) {
            done = true;
            resolve();
          }
        };
        const force = setTimeout(() => {
          server.kill('SIGKILL');
          finish();
        }, 5_000);
        server.on('exit', () => {
          clearTimeout(force);
          finish();
        });
        server.kill('SIGTERM');
      });
      try {
        execFileSync('tmux', ['-L', tmuxSocket, 'kill-server'], { stdio: 'ignore' });
      } catch {
        /* already gone */
      }
      rmSync(tmpDir, { recursive: true, force: true });
    },
    { scope: 'worker' },
  ],

  // Per-test handle on the worker's server. The server is reused; this fixture
  // just resets it to a clean slate after each test by deleting every session
  // (and its branch + worktree), so count-based assertions like "0 sessions" or
  // "exactly 2 cards" hold regardless of test order.
  weaver: async ({ server }, use) => {
    const { baseUrl, repoPath, childEnv } = server;

    const fixture: WeaverFixture = {
      baseUrl,
      repoPath,

      async seedSession(opts) {
        return (await fetchJson(`${baseUrl}/api/sessions`, {
          method: 'POST',
          body: JSON.stringify({
            goal: opts.goal,
            title: opts.title ?? opts.name,
            cwd: repoPath,
            agent: 'shell',
            name: opts.name,
            base: opts.base,
            parent_branch: opts.parent,
          }),
        })) as Session;
      },

      async seedOverlooker(opts) {
        return (await fetchJson(`${baseUrl}/api/overlookers`, {
          method: 'POST',
          body: JSON.stringify({
            name: opts.name,
            trigger: opts.trigger ?? {},
            scope: opts.scope ?? {},
            program: opts.program ?? 'builtin:status',
            params: opts.params ?? {},
            capabilities: opts.capabilities ?? ['observe', 'mark', 'escalate'],
          }),
        })) as Overlooker;
      },

      async seedIssue(session, title, body) {
        return (await fetchJson(`${baseUrl}/api/branches/${session.branch.id}/issues`, {
          method: 'POST',
          body: JSON.stringify({ title, body: body ?? '' }),
        })) as Issue;
      },

      async tagIssue(id, key, value) {
        return (await fetchJson(`${baseUrl}/api/issues/${id}/tags/${encodeURIComponent(key)}`, {
          method: 'PUT',
          body: JSON.stringify({ value }),
        })) as Issue;
      },

      async listIssues(all = false) {
        return (await fetchJson(
          `${baseUrl}/api/issues${all ? '?all=true' : ''}`,
        )) as Issue[];
      },

      async getSession(id) {
        return (await fetchJson(`${baseUrl}/api/sessions/${id}`)) as Session;
      },

      async listSessions() {
        return (await fetchJson(`${baseUrl}/api/sessions`)) as Session[];
      },

      async hook(session, event) {
        // `weaver hook` writes an `events` row keyed on the branch resolved
        // from $WEAVER_BRANCH; the loom monitor consumes it on its next tick.
        execFileSync(WEAVER_BINARY, ['hook', '--event', event], {
          env: { ...childEnv, WEAVER_BRANCH: session.branch.id },
          stdio: 'pipe',
        });
      },

      async setStatus(session, level, message) {
        // `weaver set-status <level> [message]` writes the branch's `attention`
        // tag (clearing it on `ok`) and the current-state message, recording a
        // `tag` event the monitor re-broadcasts.
        const args = ['set-status', level, ...(message ? [message] : [])];
        execFileSync(WEAVER_BINARY, args, {
          env: { ...childEnv, WEAVER_BRANCH: session.branch.id },
          stdio: 'pipe',
        });
      },

      async setTag(session, key, value, opts) {
        await fetchJson(`${baseUrl}/api/sessions/${session.id}/tags/${key}`, {
          method: 'PUT',
          body: JSON.stringify({ value, note: opts?.note, by: opts?.by }),
        });
      },

      async clearTag(session, key) {
        await fetchJson(`${baseUrl}/api/sessions/${session.id}/tags/${key}`, {
          method: 'DELETE',
        });
      },

      async mark(session, level, opts) {
        await fixture.setTag(session, 'triage', level, {
          note: opts?.note,
          by: opts?.by ?? 'manual',
        });
      },
    };

    await use(fixture);

    // Reset for the next test in this worker.
    await deleteAllSessions(baseUrl);
    await deleteAllOverlookers(baseUrl);
    await deleteAllIssues(baseUrl);
  },
});

export { expect };
