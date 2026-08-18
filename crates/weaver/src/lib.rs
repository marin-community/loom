//! Compatibility implementation for the retiring `weaver` command surface.
//!
//! Loom reuses these HTTP-only command handlers while the public commands move
//! under `loom`. Keeping one implementation makes the `weaver` binary a
//! compatibility surface rather than a second product model.

#[path = "bin/weaver.rs"]
mod cli;

pub use cli::{
    main as standalone_main, run as run_standalone, run_artifact, run_channel, run_chatlog,
    run_events, run_github_token, run_hook, run_issue, run_self, run_settings, run_status,
    run_summary, run_tag, set_client_override, ArtifactCmd, ChannelCmd, ConfigCmd, IssueCmd,
    StatusCmd, TagCmd,
};
