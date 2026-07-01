# weaver lint rules

Catalog of patterns reviewers in this repo recurrently flag — the kind of
"slop" an LLM coding agent tends to produce: code that compiles and passes
`clippy` but that a careful human would rewrite. Each rule has a short code
(`wl-...`), the condition, why it's bad, when it's nevertheless acceptable, and
a bad-pattern example.

This catalog is the input to an agent reviewer (see "Detector usage" below).
`cargo fmt` and `cargo clippy` already own whitespace, lint-level correctness,
and the mechanical Rust idioms — this file stays out of their lane and targets
the *judgement* calls they can't make: naming, shape, duplication, dead code,
and comment/test quality.

## Audience

- **Reviewer / agent**: scan a diff and emit findings in the format described
  under "Output format". See "Detector usage" for input selection.
- **Author**: search this file for the code from a finding (`wl-...`) to see
  the rule, why it matters, and when it's OK to ignore. Suppress a deliberate
  exception with a trailing `// wl-allow: <code>` comment on the cited line.

This is **not** a security review (see `/security-review`), a correctness
checker, or a formatter — `cargo fmt`, `cargo clippy`, and the frontend's
`tsc`/`eslint` already exist; stay out of whitespace, import order, line length,
and anything the compiler or `clippy` already rejects.

The examples are mostly Rust (the bulk of the repo) with TypeScript/Vue
(`crates/loom/frontend/`) where the pattern lives on that side. The *principle*
in each "Why it's bad" is language-agnostic; apply it to whichever side the
diff touches.

---

## Reuse

### `wl-reinvented-stdlib` — Hand-rolled code for something std or a dep already does

**Why it's bad:** Agents reach for a fresh loop instead of the function that
exists, because they don't know the API surface. The hand-rolled version is
longer, less tested, and drifts from the well-known semantics readers expect
(`HashMap::entry`, `Iterator::any`, `slice::contains`, `str::strip_prefix`,
serde, `Path` joins). Two implementations of "the same idea" is one too many.

**When allowed:** When the stdlib/dep version genuinely doesn't fit (different
edge-case semantics you depend on) — say so in a comment.

**Bad example:**
```rust
let mut found = false;
for w in &workers {
    if w.id == target {
        found = true;
        break;
    }
}
// workers.iter().any(|w| w.id == target)
```

---

## API shape

### `wl-bool-flag-arg` — Boolean flag selecting between behaviors

**Why it's bad:** Boolean arguments accumulate; they don't extend cleanly to a
third state and they hide intent at the call site (`spawn(true, false)` — true
what?). An enum scales to N states and reads clearly at the call site.

**When allowed:** Genuine two-state toggles where the meaning is obvious from
the parameter name and a third state is implausible (`strict: bool` on a
parser).

**Bad example:**
```rust
fn launch(branch: &str, detached: bool, adopt: bool) { ... }
// launch("x", true, false) — enum LaunchMode { Detached, Adopt, Foreground }
```

### `wl-bool-return-status` — `bool`/`Option` return for a multi-outcome operation

**Why it's bad:** A `bool` (or bare `Option`) return collapses distinct
outcomes (created / already-existed / conflict) into one bit; callers can't
distinguish them and end up reading the implementation. Return an enum or a
`Result` with a typed error.

**When allowed:** Simple binary predicates (`is_ready()`, `exists()`) and
genuine pass/fail where the only response to failure is retry.

**Bad example:**
```rust
fn adopt_session(&self, id: &str) -> bool {
    // callers can't tell "no such session" from "already adopted" from "tmux gone".
    ...
}
```

### `wl-tuple-return-shape` — Wide tuple return where a struct fits

**Why it's bad:** `(String, bool, u64)` hides positional semantics; callers
index by position and refactors break silently. Three+ fields with distinct
meanings should be a named struct.

**When allowed:** Two-tuples with obvious roles (a key/value pair), or
coordinate-like fixed tuples.

**Bad example:**
```rust
fn parse_status(line: &str) -> (String, bool, u64) {
    // what is each slot? a `struct Status { level, attention, ts }` reads itself.
    ...
}
```

### `wl-monolithic-function` — Multi-mode function that should be split

**Why it's bad:** One function with three boolean knobs encodes 2³ behaviors
the caller must reason about. Separate functions compose better and let callers
pick exactly what they need.

**When allowed:** Pre-existing public APIs where splitting would break callers.
New entry points should be narrow.

**Bad example:**
```rust
fn sync_plan(slug: &str, apply: bool, dry_run: bool, force: bool) -> Result<()> {
    // three orthogonal modes → distinct functions the caller composes.
    ...
}
```

---

## Types & data structures

### `wl-stringly-typed` — `String`/`&str` where a closed set wants an enum

**Why it's bad:** A field that only ever holds `"ok"`, `"attention"`, or
`"blocked"` typed as `String` pushes validation to every read site, invites
typos the compiler can't catch, and makes the valid set un-discoverable. Model
closed sets as an enum.

**When allowed:** Genuinely open text (a user message, a branch name), or a
value that crosses a wire/DB boundary as text — convert to an enum at the
boundary.

**Bad example:**
```rust
struct Branch {
    attention: String, // only "ok" | "attention" | "blocked" — enum Attention.
}
if branch.attention == "atention" { ... } // typo compiles; never fires.
```

### `wl-untyped-blob` — Ad-hoc map / JSON `Value` for a structured record

**Why it's bad:** A `HashMap<String, String>` or `serde_json::Value` (TS: an
inline object literal typed as `any`/`Record<string, unknown>`) for a record
with known fields skips schema validation, hides field names from the type
checker, and makes renames silent breakage. Define a struct / `interface`
once and reuse it.

**When allowed:** Truly heterogeneous payloads, or JSON at the deserialization
boundary — then immediately `serde`-decode into a struct.

**Bad example:**
```rust
let mut event = HashMap::new();
event.insert("branch".into(), branch.clone());
event.insert("level".into(), level.clone());
bus.send(event); // define `struct StatusEvent { branch, level }` and send that.
```

### `wl-bare-any` — `Box<dyn Any>` / TS `any` defeating the type system

**Why it's bad:** `dyn Any` and TS `any` switch the type checker off exactly
where the next refactor would have been caught. Use a concrete type, a trait
object with real methods, or a generic.

**When allowed:** Genuine boundary code handling unrelated types (a generic
cache value). Document why in a brief comment.

**Bad example:**
```typescript
function handle(payload: any) {   // payload is always a SessionEvent — type it.
  return payload.branch;
}
```

---

## Configuration explicitness

### `wl-env-var-vs-param` — Env var read where an explicit parameter belongs

**Why it's bad:** Reading `std::env::var` deep in a call couples it to ambient
state, can't be type-checked, and lets divergent overrides accumulate
silently. Thread the value as a parameter or `Config` field; only top-level
entry points (`main`, the CLI parser) read the environment.

**When allowed:** Operational kill-switches that must flip without a redeploy —
hoist them to one documented top-level constant, don't sprinkle `env::var`.

**Bad example:**
```rust
fn server_url() -> String {
    // take this as a Config field; main() reads the env once and passes it down.
    std::env::var("LOOM_URL").unwrap_or_else(|_| "http://localhost:7777".into())
}
```

### `wl-mutable-global` — Mutable global / module-level mutable state

**Why it's bad:** Mutable globals (`static mut`, a `Lazy<Mutex<...>>` holding
runtime config, a mutable module-level `let` in TS) scatter state and create
order-of-initialization bugs. Pass state explicitly (it's why `Db`/`Config`
are threaded through this codebase).

**When allowed:** True immutable constants and lookup tables at module scope.
The smell is *mutable* shared state, not constants.

**Bad example:**
```rust
static mut CURRENT_BRANCH: Option<String> = None; // pass it; don't stash a global.
```

### `wl-magic-constant` — Magic literal repeated without a named constant

**Why it's bad:** A literal that appears in three files drifts (one updated,
two not) and defeats a search for "where does this value come from". Hoist it
to a `const`/`static` the moment it appears twice.

**When allowed:** A literal used exactly once, inside the function that owns its
meaning.

**Bad example:**
```rust
sleep(Duration::from_millis(250)).await; // this 250 appears in 4 spots — const MONITOR_TICK.
```

### `wl-config-not-threaded` — Config knob exposed but ignored by the consumer

**Why it's bad:** A setting surfaced at the CLI/config layer but hard-coded at
the point of use (a `--port` flag while the server binds a literal) is worse
than no knob — operators trust it and get burned.

**When allowed:** Genuinely cosmetic config (a label) where divergence is
harmless. Anything affecting behavior must thread.

**Bad example:**
```rust
struct Config { port: u16 }       // parsed from --port
fn serve(_cfg: &Config) {
    bind("127.0.0.1:7777");        // hard-coded; cfg.port is decoration.
}
```

---

## Defensive code

### `wl-swallowed-error` — Error silently discarded or defaulted away

**Why it's bad:** `let _ = fallible();`, `.ok()`, `.unwrap_or_default()` on a
real operation, or an empty `catch {}` throws away the failure. The caller
can't tell "succeeded" from "failed and we ignored it", and a genuine bug hides
for months. Propagate with `?` or handle the error explicitly.

**When allowed:** Best-effort cleanup where failure truly doesn't matter
(closing an already-broken socket) — and then say so in a comment, and log it.
Background loops that must survive a per-item failure: log the error with
context, don't swallow it.

**Bad example:**
```rust
let _ = db.record_event(&ev).await; // did it persist? nobody knows. Use `?`.
```
```typescript
try { await api.patch(id, body) } catch {}  // UI silently no-ops on failure.
```

### `wl-reckless-unwrap` — `.unwrap()` / `.expect()` on a fallible op in non-test code

**Why it's bad:** `.unwrap()` turns a recoverable error into a process-killing
panic, often on exactly the I/O (network, tmux, git, DB) that fails in
production. This crate returns `Result`/`anyhow` everywhere — propagate with `?`
and add context.

**When allowed:** Tests and examples. Production invariants that genuinely
cannot fail — and then `expect("why this can't fail")` documenting the
invariant, not a bare `unwrap()`. Lock poisoning (`.lock().unwrap()`) is
idiomatic.

**Bad example:**
```rust
let out = Command::new("git").arg("status").output().unwrap(); // git can fail — use `?`.
```

### `wl-guard-after-use` — Defensive guard placed after the operation it should protect

**Why it's bad:** A `None`/type check *inside* the `else`/error path runs after
the dereference already failed — shutting the gate after the horse bolted.
Guard at the boundary, before the use.

**When allowed:** Never. Move the guard above the use.

**Bad example:**
```rust
let name = map.get("name").unwrap(); // panics here…
if map.contains_key("name") { ... }  // …guard is too late.
```

---

## Dead & speculative code

### `wl-unused-param` — Unused function parameter

**Why it's bad:** An unused parameter implies a contract that doesn't exist and
misleads callers into passing something that's silently ignored. Delete it.

**When allowed:** Required by a trait/callback signature — then prefix with `_`
(`_cx`) to mark the intent explicitly.

**Bad example:**
```rust
fn render_header(branch: &Branch, repo: &str, verbose: bool) -> String {
    // `repo` and `verbose` are never read — drop them.
    format!("{}", branch.name)
}
```

### `wl-speculative-abstraction` — Trait / generic / enum with one implementation

**Why it's bad:** A trait, generic parameter, or enum introduced "in case we
add more later" costs reader attention now and pays back only at the second
case — by which point the real shape is known and easy to extract. The agent
tell is a `trait FooProvider` with a single `impl`, or an enum with one
variant.

**When allowed:** When the second implementation is already in flight, or the
abstraction is a deliberate, documented design point.

**Bad example:**
```rust
trait SessionStore {                 // one impl, ever
    fn get(&self, id: &str) -> Option<Session>;
}
impl SessionStore for SqliteStore { ... }
// use SqliteStore directly until a second store actually exists.
```

### `wl-unrequested-backcompat` — Backwards-compat shim with no remaining callers

**Why it's bad:** Agents reflexively keep the old function as a thin wrapper, a
`#[deprecated]` alias, or a `field_old` next to `field`, "to be safe" — when a
`grep` shows nothing else calls it. In a repo you fully control, that's pure
debt: just update the callers and delete the old path.

**When allowed:** A genuinely external/public API with out-of-tree consumers,
or a real migration window with the removal PR linked. State which.

**Bad example:**
```rust
// keep old signature for compatibility
fn set_status(level: &str) { set_status_with_message(level, None) } // no other caller — delete it.
```

### `wl-rollout-scaffolding` — Knob added "just for the rollout"

**Why it's bad:** Flags added "to stage safely, remove after testing" almost
never get removed; they become permanent surface area reviewers must reason
about.

**When allowed:** Only with an explicit removal trigger and owner in a comment
("delete once all sessions on ≥0.4"). Without one, don't add it.

**Bad example:**
```rust
let use_new_monitor = std::env::var("NEW_MONITOR").is_ok(); // nervous-rollout flag, no removal plan.
```

### `wl-obsolete-after-refactor` — Code left unreachable by an earlier change

**Why it's bad:** A branch or helper that handled the old path still sits there
after the new path took over. Dead branches confuse readers and the next
refactor wastes time deciding whether they're load-bearing.

**When allowed:** A compatibility shim tied to a named removal trigger.

**Bad example:**
```rust
fn status_for(session: &Session) -> Status {
    if session.legacy_pidfile.is_some() {   // pidfiles were removed two refactors ago — dead.
        return Status::from_pidfile(session);
    }
    Status::from_tmux(session)
}
```

### `wl-add-then-remove` — Within-branch add-then-remove churn

**Why it's bad:** One commit adds a field/column/flag and a later commit on the
same branch removes it. The intermediate state never shipped, so the addition
is pure churn — a reader has to mentally cancel two changes. Rebase the
addition out.

**When allowed:** Never. Squash it away.

**Bad example:**
```
0007_add_worker_heartbeat.sql      # adds column
0009_drop_worker_heartbeat.sql     # drops what 0007 added, same branch
```

### `wl-commented-out-code` — Leftover commented-out code

**Why it's bad:** Commenting code out instead of deleting it is a classic agent
reflex ("might need it later"). It rots immediately, confuses readers about
whether it's significant, and git already remembers it. Delete it.

**When allowed:** A short, deliberately-illustrative snippet in a doc comment,
clearly framed as an example.

**Bad example:**
```rust
// let old = compute_legacy(x);
// return old.merge(new);
return compute(x);
```

### `wl-debug-leftover` — Leftover debug output

**Why it's bad:** `dbg!`, ad-hoc `println!`/`eprintln!`, and `console.log`
left in after debugging are noise that leaks to users and clutters logs. Use
the project's logging (`tracing`) at a deliberate level, or remove it.

**When allowed:** Intentional user-facing CLI output, and `tracing` calls at a
considered level.

**Bad example:**
```rust
dbg!(&session);                 // debugging leftover
println!("got here");           // remove or convert to tracing::debug!
```
```typescript
console.log("payload", payload) // remove before commit
```

---

## Duplication

### `wl-duplicate-logic` — Same logic block in two or more places

**Why it's bad:** Two copies of an algorithm drift: a fix to one is silently
absent in the other. Extract a shared function.

**When allowed:** Two sites in deliberately isolated modules where coupling
them would create a worse dependency. Three+ copies are never acceptable.

**Bad example:**
```rust
// in web.rs and again in bin/loom.rs:
let key = format!("{}:{}", repo_root, branch);
let id = sha256(&key)[..12].to_string();
// extract `fn branch_key(repo_root, branch) -> String`.
```

### `wl-parallel-impl` — Two production functions doing the same operation

**Why it's bad:** A "legacy" builder sitting next to the new one, or
`submit`/`enqueue` differing only in their input source, is source-cloned
production code. Drift here shows up in production, not in tests.

**When allowed:** A migration window where both paths are intentionally live,
with the deletion PR linked.

**Bad example:**
```rust
fn event_from_hook(h: &Hook) -> Event { ... }
fn event_from_legacy_hook(h: &Hook) -> Event { ... } // same builder, two names.
```

### `wl-duplicate-constant` — Constant duplicated when a canonical source exists

**Why it's bad:** The default port, the socket path, or the set of valid
statuses re-declared in three modules will drift when one changes. Derive from
one canonical definition.

**When allowed:** Two genuinely unrelated values that share a literal today by
coincidence.

**Bad example:**
```rust
// in server.rs
const DEFAULT_PORT: u16 = 7777;
// in client.rs
let port = 7777; // import DEFAULT_PORT.
```

---

## Naming

### `wl-utils-module` — Module named `utils` / `helpers` / `misc`

**Why it's bad:** Generic `utils.rs` / `helpers.ts` modules become dumping
grounds and tell readers nothing about their contents. Name the module for what
it does (`branch_key.rs`, `time_format.ts`).

**When allowed:** Rarely — even then prefer a descriptive name.

**Bad example:**
```
crates/loom/src/utils.rs   # what's in it? rename for its actual responsibility.
```

### `wl-misleading-name` — Name doesn't match what it does or returns

**Why it's bad:** `get_branch` that *creates* one, or `wall_ms` that holds
seconds, mislead every future reader and bugs follow. Names are load-bearing
documentation.

**When allowed:** Never knowingly. Rename on sight.

**Bad example:**
```rust
fn validate_branch(name: &str) -> Branch { create_branch(name) } // it creates, not validates.
```

### `wl-vestigial-qualifier` — `_v2` / `_new` / `_legacy` / `_impl2` with no surviving contrast

**Why it's bad:** These suffixes imply two variants when only one remains. They
propagate (callers copy the name) and the contrast they referenced is already
gone. Drop the qualifier.

**When allowed:** The contrasting variant genuinely still exists and isn't
slated for removal.

**Bad example:**
```rust
fn launch_session_v2(...) { ... } // there is no launch_session — it's just launch_session.
```

### `wl-cryptic-abbrev` — Cryptic abbreviation in a name

**Why it's bad:** `exct`, `sess_mgr`, `brnch` save a few keystrokes once and
cost readability forever. Spell it out.

**When allowed:** Domain-standard short forms (`id`, `url`, `http`, `db`,
`cfg`, `ctx`).

**Bad example:**
```rust
fn upd_sess_attn(s: &mut Sess) { ... } // update_session_attention(session)
```

### `wl-unit-suffix` — Bare numeric duration/size without a unit

**Why it's bad:** `timeout: u64` — milliseconds? seconds? The unit lives only
in the author's head and call sites guess. Use a typed `Duration`, or suffix
the unit (`timeout_ms`, `size_bytes`).

**When allowed:** Counts and indices that have no unit.

**Bad example:**
```rust
fn wait(&self, timeout: u64) { ... } // timeout_ms, or timeout: Duration.
```

---

## Comments & documentation

### `wl-restating-comment` — Comment paraphrases the line below

**Why it's bad:** A comment that restates the code is pure noise and rots first.
Comments are for the *why* and the subtle, not for narrating the obvious.

**When allowed:** Never. If you can't say what's lost by deleting it, delete it.

**Bad example:**
```rust
// increment the counter
counter += 1;
```

### `wl-trivial-doc` — Doc comment narrates a self-evident one-liner

**Why it's bad:** `/// Returns the id.` on `fn id(&self) -> &str` says nothing
the signature didn't. It's LLM filler.

**When allowed:** Public API items where the doc adds a real contract detail.

**Bad example:**
```rust
/// Get the branch name.
fn name(&self) -> &str { &self.name }
```

### `wl-verbose-doc` — Multi-paragraph doc on a trivial body

**Why it's bad:** A three-line function with a four-paragraph doc comment is a
hallmark of generated code: it buries the one fact that matters (if any) and
sets a maintenance trap. Internal helpers want one line at most.

**When allowed:** Genuinely complex public APIs whose contract needs the space.

**Bad example:**
```rust
/// Normalize a string.
///
/// This function takes a string and returns a normalized form. The
/// normalization process trims surrounding whitespace and lowercases all
/// characters, producing a canonical representation suitable for comparison...
fn normalize(s: &str) -> String { s.trim().to_lowercase() }
```

### `wl-impl-narration-doc` — Doc narrates implementation/history, not the caller contract

**Why it's bad:** A doc comment states what a function does *for its caller* —
the contract. Narrating *how* it's implemented ("does a single atomic UPDATE",
"used by the monitor and the CLI"), or self-congratulation ("single source of
truth"), is noise that rots when the implementation changes — and `grep` finds
the callers. If an implementation detail is load-bearing, put it in an inline
comment next to the code it explains.

**When allowed:** When the detail *is* the contract (an ordering or idempotency
guarantee the caller must rely on). State the behavior, not the mechanism.

**Bad example:**
```rust
/// Resolve the branch. Implemented as a single sqlx query joining sessions and
/// branches so there's no TOCTOU window. Called by hook(), summary(), and serve().
fn resolve_branch(...) -> Result<Branch> { ... } // how + caller list — delete both.
```

### `wl-pr-reference-comment` — Comment names a PR / issue / phase / kata

**Why it's bad:** "Added for #42 / the v0.3 rollout / phase B" belongs in the
commit message and `git blame`. In source it rots — a reader six months later
can't recover the context and the reference misleads.

**When allowed:** A durable link the comment is *pointing to* (a stable issue
URL, an ADR path), not transient project vocabulary.

**Bad example:**
```rust
// added for the schema-migration work in #31
let version = read_schema_version(&db)?;
```

### `wl-bare-todo` — `TODO` without an owner or trigger

**Why it's bad:** A bare `TODO` accumulates and signals work without enabling
it. An actionable TODO names the trigger ("once migrations land") or an owner.

**When allowed:** Throwaway scripts. Committed code: name the trigger.

**Bad example:**
```rust
// TODO: clean this up
```

### `wl-stale-comment` — Comment or doc describes superseded behavior

**Why it's bad:** Readers trust comments. One that says "word-level shingling"
after the code switched to character-level, or a doc describing a removed
parameter, actively misleads and the next refactor misses it.

**When allowed:** Never knowingly — update or delete on sight.

**Bad example:**
```rust
// poll every second
sleep(Duration::from_millis(250)).await; // comment is stale: it's 250ms.
```

### `wl-doc-contradicts-impl` — Doc promises behavior the code doesn't deliver

**Why it's bad:** The most expensive stale doc. A `fn` documented "read-only"
that opens the DB for writes corrupts data for callers who trusted the
contract.

**When allowed:** Never.

**Bad example:**
```rust
/// Read-only: inspects the session without mutating state.
fn inspect(db: &Db, id: &str) -> Result<Session> {
    db.execute("UPDATE sessions SET ...")?; // writes — contradicts the doc.
    ...
}
```

---

## Test quality

### `wl-slop-test` — Test asserts on incidentals, not behavior

**Why it's bad:** The catch-all for low-value tests: they *look* like coverage
but validate nothing real, or pin to incidental detail that breaks on a
harmless edit while real regressions slip through. If you can't name the
production bug the test would catch, it's slop. A test is slop when it:

- **Asserts on a log line's text** — `assert!(logs.contains("retrying"))`. A
  log line isn't a contract.
- **Asserts a token appears in an assembled command/argv** —
  `assert!(cmd.contains("--detach"))`. Tests how the command was built, not what
  it does; a flag rename or a wrong value beside the right flag both slip
  through.
- **Asserts on exact human-readable copy** — a status string, a formatted
  error. Breaks on any wording tweak.
- **Asserts almost nothing** — runs a path and only checks it didn't panic /
  returned `Some` / a mock was called. Coverage with no behavioral claim.
- **Is tautological** — recomputes the same expression the code does, so it
  passes by construction.
- **Checks what the compiler already guarantees** — asserting a value has the
  type the signature already declares.

**When allowed:** When the string/structure genuinely *is* the contract
(machine-readable CLI output, a wire/serialization format, a log line a
downstream tool parses) — then assert on the *parsed/structured* value, not the
rendered text, with a comment saying why.

**Bad example:**
```rust
#[test]
fn test_launch() {
    let cmd = build_launch_cmd(&cfg);
    assert!(cmd.contains("--detach")); // reorder / --detach=true breaks it; value never checked
}
```

### `wl-sleep-in-test` — Real sleep in a test body

**Why it's bad:** `thread::sleep` / `tokio::time::sleep` to "wait for" the SUT
races it instead of controlling it; the test goes flaky under CI load. Poll a
condition with a deadline, inject a clock, or use `tokio::time` pause/advance.

**When allowed:** A genuinely time-bound integration test, marked as such, with
a comment naming what the wait is for.

**Bad example:**
```rust
submit_event(&bus);
std::thread::sleep(Duration::from_millis(500)); // racy — poll for the event with a deadline.
assert_eq!(received.len(), 1);
```

### `wl-over-mocked-test` — Test mocks or re-implements the thing under test

**Why it's bad:** A test double that mirrors the SUT's own logic passes when the
SUT is wrong in the same way — it isolates nothing. Mocking every collaborator
and then asserting the mocks were called the way you wired them tests your
wiring, not the behavior.

**When allowed:** Recording adapters that observe inputs/outputs without
re-deriving them; mocking a true external boundary (network, clock).

**Bad example:**
```rust
struct FakeMonitor;
impl FakeMonitor {
    fn tick(&self) -> Status {
        // 30 lines re-deriving the real monitor's status logic — mirrors the SUT.
        ...
    }
}
```

---

## Detector usage

For an agent running this catalog against a diff.

### Inputs

Pick the diff that applies, typically the current branch versus its merge-base
with `main`, unless the caller requests a tighter set:

- Feature branch (the default `scripts/lint-review.py` scope): `git diff $(git merge-base origin/main HEAD)...HEAD -- '*.rs' '*.ts' '*.vue'`
- Staged only: `git diff --cached -- '*.rs' '*.ts' '*.vue'`
- A specific PR: `gh pr diff <number> -- '*.rs' '*.ts' '*.vue'`
- A named file or two: read the file in full.

If the diff is empty, emit nothing and stop. Scan added/modified hunks plus
enough surrounding context to judge intent (usually the enclosing
function/`impl`/component). Do not flag pre-existing code in unchanged regions.

Out of scope: generated output (`crates/loom/static/dist/**`, `Cargo.lock`,
`package-lock.json`, any `*_pb2`/codegen), and anything `cargo fmt` / `clippy` /
`tsc` already enforce. Security findings belong in `/security-review`.

**Never flag scale or performance.** Weaver is a ~hundred-row SQLite app: N+1
queries, missing indexes, denormalization, join cost, allocation/clone overhead,
and the like do not apply. Don't emit them.

### Suppression markers

A finding is suppressed when the cited line carries a trailing
`// wl-allow: <code>` comment naming the rule. A suppressed line is an
author-approved exception — do not emit a finding for it.

```rust
let _ = best_effort_cleanup(); // wl-allow: wl-swallowed-error — socket already closed
```

### Confidence

Every finding carries a confidence in `[0.0, 1.0]`:

- `≥0.9` — near-verbatim match to a rule; the review comment writes itself.
- `0.7–0.9` — fits the rule's intent; some context uncertainty.
- `<0.7` — do not emit.

Do not pad. Empty output is correct. **False positives are the failure mode
that erodes trust** — when uncertain, suppress. If you wouldn't bet $1 the
finding is valid, score it below 0.7.

### Overlap precedence

When two rules touch one line, emit the more specific one alone:

- A comment that is both wrong *and* restates the code → `wl-stale-comment`.
- A `_v2`/`_legacy` name whose contrast is gone → `wl-vestigial-qualifier`, not
  `wl-misleading-name`.
- A wrapper kept for compat with no callers → `wl-unrequested-backcompat`, not
  `wl-parallel-impl`.

If a line legitimately violates two *unrelated* rules (a `dyn Any` return on a
`_v2` function), emit both.

### Output format

One finding per line, nothing else — no preamble, summary, JSON, or fenced
blocks:

```
<path>:<line>: <code> (<confidence>) <message>
```

- `<path>` — repo-relative, forward slashes.
- `<line>` — 1-indexed in the file as it exists post-change.
- `<code>` — the `wl-...` code from this file.
- `<confidence>` — two decimals, e.g. `0.82`.
- `<message>` — ≤200 chars. State the concern; do not propose a fix.

Worked examples:

```
crates/loom/src/monitor.rs:142: wl-reckless-unwrap (0.90) .unwrap() on git output; tmux/git failures will panic the monitor
crates/weaver-core/src/tags.rs:31: wl-stringly-typed (0.85) Tag.value: String holds a closed set for loud keys; model as an enum
crates/loom/frontend/src/api.ts:88: wl-swallowed-error (0.88) empty catch hides PATCH failure from the UI
crates/loom/src/utils.rs:1: wl-utils-module (0.75) generic utils module; name it for its responsibility
```

If the diff has no in-scope files, emit nothing — no "no findings" message.

### Self-evaluation

- **Precision over recall.** One false positive and the author trusts the tool
  less. When uncertain, suppress.
- **Stay in scope.** Only the rules in this file. Don't moonlight as a security,
  performance, or style reviewer.
- **Anchor in real shapes.** A reader at the cited line should immediately see
  why you flagged it. If you're reaching, suppress.
