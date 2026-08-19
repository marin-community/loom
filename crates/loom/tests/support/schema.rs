//! A migrated-schema template, shared by every suite that boots a server on a
//! throwaway `WEAVER_HOME`. Pulled in with `#[path]` like `support/tapestry.rs`
//! — separate integration-test targets can't share code any other way.
//!
//! `loom::db::connect` on an empty file replays the whole ordered migration set
//! (weaver-core's stream, then loom's), each migration in its own transaction.
//! That costs ~250ms, and these suites are `#[serial]`, so it is ~250ms of dead
//! wall-clock in front of *every* test before it does any work of its own.
//!
//! The migration run is deterministic, so run it once per test process and hand
//! each test a byte-identical copy of the result. What a test connects to is
//! still the real migrated schema — it is literally the output of the real
//! runner — and the from-scratch migrate path keeps its own coverage in the
//! `weaver-core` and `loom-store` unit tests (plus the one run this template
//! itself performs).

use std::sync::OnceLock;

/// Plant the migrated schema at `WEAVER_HOME`'s database path, so the
/// `db::connect` that follows finds every migration already applied and only
/// has to open the pool.
///
/// Call it after pointing `WEAVER_HOME` at the test's temp home and before
/// connecting; the path is resolved the same way `connect`'s caller resolves it.
pub fn seed_migrated_db() {
    let path = loom::db::default_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("creating the test weaver home");
    }
    std::fs::write(&path, template()).expect("planting the migrated schema template");
}

/// The bytes of a freshly migrated database, built once per test process.
fn template() -> &'static [u8] {
    static TEMPLATE: OnceLock<Vec<u8>> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        // Build on a dedicated thread with its own runtime: callers run inside a
        // `#[tokio::test]` runtime that is torn down at the end of the test, and
        // the template must outlive whichever test happened to be first.
        std::thread::spawn(|| {
            let dir = tempfile::tempdir().expect("schema template directory");
            let path = dir.path().join("weaver.db");
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("schema template runtime")
                .block_on(async {
                    let pool = loom::db::connect(&path)
                        .await
                        .expect("migrating the schema template");
                    // Closing the last connection checkpoints the WAL back into
                    // the main file, so these bytes are the whole database.
                    pool.close().await;
                });
            std::fs::read(&path).expect("reading the schema template")
        })
        .join()
        .expect("schema template thread")
    })
}
