import { defineConfig } from '@playwright/test';

// One loom server is booted per *worker* (see fixtures/weaver.ts) and reused
// across that worker's tests; the per-test fixture wipes sessions between tests
// so each starts clean. Workers are fully isolated — own WEAVER_HOME/db, port,
// and a private tmux socket — so tests run in parallel safely. `globalSetup`
// builds the binaries + SPA once up front so workers don't race on `cargo build`.
// Tests are deterministic with the `shell` agent.
export default defineConfig({
  testDir: './tests',
  globalSetup: './fixtures/global-setup.ts',
  timeout: 60_000,
  expect: { timeout: 15_000 },
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 2 : 4,
  fullyParallel: true,
  reporter: [['list']],
  use: {
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
});
