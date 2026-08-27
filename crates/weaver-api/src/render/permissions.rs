//! Text rendering for brokered credentials.

use crate::dto::GithubTokenView;
use crate::operations::permissions;
use crate::operations::{NoView, Render};

impl Render for permissions::github::token::Op {
    /// The token and nothing else. A git credential helper and the `gh` wrapper
    /// both capture this with `$(...)`, so a JSON envelope would be read as the
    /// credential.
    fn text(output: &GithubTokenView, _: &NoView) -> String {
        output.token.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default renderer would print a JSON envelope, and the callers here
    /// are `token="$(loom github-token)"` in a git credential helper and in the
    /// `gh` wrapper — both would hand `{"token":"…"}` to GitHub as the password.
    #[test]
    fn a_brokered_token_renders_as_itself() {
        let output = GithubTokenView {
            token: "ghs_example".to_string(),
        };
        assert_eq!(
            permissions::github::token::Op::text(&output, &NoView),
            "ghs_example"
        );
    }
}
