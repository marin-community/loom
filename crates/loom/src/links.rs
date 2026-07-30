//! Dashboard deep links — the URLs a person opens to look at something loom is
//! running.
//!
//! These are pure string formatting over the SPA's routes, deliberately free of
//! any dependency on the HTTP layer that serves them: the Slack and GitHub
//! integrations post these links without going anywhere near a request. Pair
//! `base` with [`crate::auth::public_base`] so the link resolves off-box.

/// The page a person opens to watch a session.
pub fn session_url(base: &str, session_id: &str) -> String {
    format!("{}/s/{session_id}", base.trim_end_matches('/'))
}

/// The page a person opens to read an artifact (`/s/:id/artifacts/:name` in the
/// SPA router). `key` is any session key — the `$WEAVER_BRANCH` an agent carries
/// resolves fine.
pub fn artifact_url(base: &str, key: &str, name: &str) -> String {
    format!("{}/s/{key}/artifacts/{name}", base.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_slash_on_the_base_does_not_double_up() {
        assert_eq!(
            session_url("http://host:8080/", "s1"),
            "http://host:8080/s/s1"
        );
        assert_eq!(
            artifact_url("http://host:8080/", "s1", "notes.md"),
            "http://host:8080/s/s1/artifacts/notes.md"
        );
    }
}
