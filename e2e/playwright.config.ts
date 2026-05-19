import { defineConfig } from '@playwright/test';

// Each test file gets its own weaver server + temp state (see fixtures/weaver.ts),
// so a single worker keeps tmux sessions (which are machine-global) under control
// and makes failures easy to read. Tests are deterministic with the `shell` agent.
export default defineConfig({
  testDir: './tests',
  timeout: 60_000,
  expect: { timeout: 15_000 },
  retries: process.env.CI ? 1 : 0,
  workers: 1,
  fullyParallel: false,
  reporter: [['list']],
  use: {
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [{ name: 'chromium', use: { browserName: 'chromium' } }],
});
