import { test as base, expect } from '@playwright/test';
import { type ChildProcess, execFileSync, spawn } from 'child_process';
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';

// Repo layout: this file lives at <weaver>/e2e/fixtures/weaver.ts
const WEAVER_ROOT = join(__dirname, '..', '..');
const BINARY = join(WEAVER_ROOT, 'target', 'debug', 'weaver');
const FRONTEND_DIR = join(WEAVER_ROOT, 'frontend');
const DIST_INDEX = join(WEAVER_ROOT, 'static', 'dist', 'index.html');

/** A workspace object as returned by the weaver REST API. */
export interface Workspace {
  id: string;
  name: string;
  goal: string;
  description: string;
  status: string;
  repo_root: string;
  work_dir: string;
  branch: string;
  base_branch: string;
  tmux_session: string;
  agent_kind: string;
  github_repo: string | null;
  github_issue: number | null;
  created_at: string;
  updated_at: string;
  last_activity_at: string;
  summary_updated_at: string | null;
}

export interface SeedOpts {
  goal: string;
  name?: string;
  base?: string;
}

export interface WeaverFixture {
  /** Base URL of the running weaver server, e.g. http://127.0.0.1:NNNN */
  baseUrl: string;
  /** Path to the throwaway git repo (one commit on `main`) used as `cwd`. */
  repoPath: string;
  /** Create a workspace directly via the API using the `shell` agent. */
  seedWorkspace(opts: SeedOpts): Promise<Workspace>;
  /** GET /api/workspaces/{id}. */
  getWorkspace(id: string): Promise<Workspace>;
  /** GET /api/workspaces. */
  listWorkspaces(): Promise<Workspace[]>;
  /** POST /api/hook to flip a workspace status (working|waiting|idle). */
  hook(id: string, event: 'working' | 'waiting' | 'idle'): Promise<void>;
  /** Poll /api/workspaces/{id}/pane until `marker` appears (or throw). */
  waitForPane(id: string, marker: string, timeoutMs?: number): Promise<string>;
}

/** Ensure the weaver binary and the Vue frontend bundle both exist. */
function ensureBuilt() {
  const needBinary = !existsSync(BINARY);
  const needFrontend = !existsSync(DIST_INDEX);
  if (needBinary || needFrontend) {
    // A full `cargo build` also builds the frontend into static/dist.
    execFileSync('cargo', ['build'], {
      cwd: WEAVER_ROOT,
      stdio: 'inherit',
      env: process.env,
    });
  }
  if (!existsSync(BINARY)) {
    throw new Error(`weaver binary missing after build: ${BINARY}`);
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

    // Bind to a random free port (0) and parse the actual port from stdout.
    const server: ChildProcess = spawn(BINARY, ['serve', '--addr', '127.0.0.1:0'], {
      env: {
        ...process.env,
        WEAVER_HOME: weaverHome,
        WEAVER_DB: dbPath,
        RUST_LOG: 'weaver=warn',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });

    let baseUrl = '';
    let serverLog = '';
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(
        () => reject(new Error(`weaver did not start in 20s. Output:\n${serverLog}`)),
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
          reject(new Error(`weaver exited with code ${code} before listening:\n${serverLog}`));
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
    if (!healthy) throw new Error(`weaver /api/health never returned ok:\n${serverLog}`);

    // UI-created workspaces should use a plain shell, never the real claude CLI.
    await fetchJson(`${baseUrl}/api/settings`, {
      method: 'POST',
      body: JSON.stringify({ key: 'agent.default', value: 'shell' }),
    });

    const fixture: WeaverFixture = {
      baseUrl,
      repoPath,

      async seedWorkspace(opts) {
        return (await fetchJson(`${baseUrl}/api/workspaces`, {
          method: 'POST',
          body: JSON.stringify({
            goal: opts.goal,
            cwd: repoPath,
            agent: 'shell',
            name: opts.name,
            base: opts.base,
          }),
        })) as Workspace;
      },

      async getWorkspace(id) {
        return (await fetchJson(`${baseUrl}/api/workspaces/${id}`)) as Workspace;
      },

      async listWorkspaces() {
        return (await fetchJson(`${baseUrl}/api/workspaces`)) as Workspace[];
      },

      async hook(id, event) {
        await fetchJson(`${baseUrl}/api/hook`, {
          method: 'POST',
          body: JSON.stringify({ workspace: id, event }),
        });
      },

      async waitForPane(id, marker, timeoutMs = 15_000) {
        const start = Date.now();
        let content = '';
        while (Date.now() - start < timeoutMs) {
          try {
            const pane = (await fetchJson(`${baseUrl}/api/workspaces/${id}/pane`)) as {
              content: string;
            };
            content = pane.content ?? '';
            if (content.includes(marker)) return content;
          } catch {
            /* retry */
          }
          await new Promise((r) => setTimeout(r, 250));
        }
        throw new Error(
          `marker "${marker}" not seen in pane of ${id} within ${timeoutMs}ms. Last pane:\n${content}`,
        );
      },
    };

    await use(fixture);

    // --- Teardown: delete every workspace (kills machine-global tmux sessions),
    // then stop the server and remove temp dirs.
    try {
      const all = (await fetchJson(`${baseUrl}/api/workspaces`)) as Workspace[];
      for (const ws of all) {
        try {
          await fetch(`${baseUrl}/api/workspaces/${ws.id}?keep_branch=false`, {
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
