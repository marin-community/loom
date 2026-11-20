import { defineConfig } from '@playwright/test';

// One loom server is booted per *worker* (see fixtures/weaver.ts) and reused
// across that worker's tests; the per-test fixture wipes sessions between tests
// so each starts clean. Workers are fully isolated — own WEAVER_HOME/db and port
// (the home also scopes the tapestry sockets) — so tests run in parallel safely.
// `globalSetup`
// builds the binaries + SPA once up front so workers don't race on `cargo build`.
// Tests are deterministic with the `shell` agent.

// PLAYWRIGHT_WORKERS overrides the worker count (each worker boots a full
// Chromium against its own loom server, so memory scales with it). Invalid
// values fall back to the CI/local default rather than crashing the runner.
function workerCount(): number {
  const parsed = Number(process.env.PLAYWRIGHT_WORKERS);
  if (Number.isInteger(parsed) && parsed > 0) return parsed;
  return process.env.CI ? 2 : 4;
}

export default defineConfig({
  testDir: './tests',
  globalSetup: './fixtures/global-setup.ts',
  timeout: 60_000,
  expect: { timeout: 15_000 },
  retries: process.env.CI ? 1 : 0,
  workers: workerCount(),
  fullyParallel: true,
  reporter: [['list']],
  use: {
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
});
