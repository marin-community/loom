import { ensureBuilt } from './weaver';

// Runs once before any worker starts (playwright.config.ts → globalSetup). The
// per-worker `server` fixture assumes the binaries + SPA bundle already exist, so
// building here — serially, before fan-out — keeps parallel workers from racing
// on a concurrent `cargo build` / rspack write into the shared target tree.
export default function globalSetup() {
  ensureBuilt();
}
