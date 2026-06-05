import { test as base, expect } from '@playwright/test';
import { type ChildProcess, execFileSync, spawn } from 'child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';

// Repo layout: this file lives at <weaver>/e2e/fixtures/weaver.ts
const WEAVER_ROOT = join(__dirname, '..', '..');
const LOOM_BINARY = join(WEAVER_ROOT, 'target', 'debug', 'loom');
const WEAVER_BINARY = join(WEAVER_ROOT, 'target', 'debug', 'weaver');
const FRONTEND_DIR = join(WEAVER_ROOT, 'crates', 'loom', 'frontend');
const DIST_INDEX = join(WEAVER_ROOT, 'crates', 'loom', 'static', 'dist', 'index.html');

/** The branch-level fields embedded in a SessionView. */
export interface Branch {
  id: string;
  name: string;
  title: string;
  goal: string;
  description: string;
  /** Agent-declared attention level: 'ok' | 'attention' | 'blocked'. */
  attention: string;
  attention_note: string;
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
  summary_updated_at: string | null;
  created_at: string;
  updated_at: string;
  branch: Branch;
}

export interface SeedOpts {
  goal: string;
  /** Title; defaults to `name` so the detail heading is predictable in tests. */
  title?: string;
  name?: string;
  base?: string;
}

export interface WeaverFixture {
  /** Base URL of the running loom server, e.g. http://127.0.0.1:NNNN */
  baseUrl: string;
  /** Path to the throwaway git repo (one commit on `main`) used as `cwd`. */
  repoPath: string;
  /** Create a session directly via the API using the `shell` agent. */
  seedSession(opts: SeedOpts): Promise<Session>;
  /** GET /api/sessions/{id}. */
  getSession(id: string): Promise<Session>;
  /** GET /api/sessions. */
  listSessions(): Promise<Session[]>;
  /** Flip a session's status by writing a hook event row via `weaver hook`. */
  hook(session: Session, event: 'working' | 'waiting' | 'idle'): Promise<void>;
  /** Declare the agent's attention level + note via `weaver status`. */
  setStatus(
    session: Session,
    level: 'ok' | 'attention' | 'blocked',
    note?: string,
  ): Promise<void>;
}

/** Ensure the loom binary and the Vue frontend bundle both exist. */
function ensureBuilt() {
  const needBinary = !existsSync(LOOM_BINARY) || !existsSync(WEAVER_BINARY);
  const needFrontend = !existsSync(DIST_INDEX);
  if (needBinary || needFrontend) {
    // A full `cargo build` also builds the frontend into static/dist.
    execFileSync('cargo', ['build'], {
      cwd: WEAVER_ROOT,
      stdio: 'inherit',
      env: process.env,
    });
  }
  if (!existsSync(LOOM_BINARY)) {
    throw new Error(`loom binary missing after build: ${LOOM_BINARY}`);
  }
  if (!existsSync(WEAVER_BINARY)) {
    throw new Error(`weaver binary missing after build: ${WEAVER_BINARY}`);
  }
  if (!existsSync(DIST_INDEX)) {
    // Binary built with WEAVER_SKIP_FRONTEND, or stale: build the SPA directly.
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

export const test = base.extend<{ weaver: WeaverFixture }>({
  weaver: async ({}, use) => {
    ensureBuilt();

    const tmpDir = mkdtempSync(join(tmpdir(), 'weaver-e2e-'));
    const weaverHome = join(tmpDir, 'home');
    const dbPath = join(tmpDir, 'db.sqlite');
    const repoPath = join(tmpDir, 'repo');
    mkdirSync(weaverHome, { recursive: true });
    makeRepo(repoPath);

    // Per-test env: every spawned process (loom + weaver hooks) sees the same
    // WEAVER_HOME / WEAVER_DB so they read and write the same database.
    const childEnv = {
      ...process.env,
      WEAVER_HOME: weaverHome,
      WEAVER_DB: dbPath,
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
          }),
        })) as Session;
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

      async setStatus(session, level, note) {
        // `weaver status <level> [note]` writes the branch's attention fields
        // directly and records an `attention` event the monitor re-broadcasts.
        const args = ['status', level, ...(note ? [note] : [])];
        execFileSync(WEAVER_BINARY, args, {
          env: { ...childEnv, WEAVER_BRANCH: session.branch.id },
          stdio: 'pipe',
        });
      },
    };

    await use(fixture);

    // --- Teardown: delete every session (kills machine-global tmux sessions),
    // then stop the server and remove temp dirs.
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

    rmSync(tmpDir, { recursive: true, force: true });
  },
});

export { expect };
