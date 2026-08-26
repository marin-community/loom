//! Text rendering for named session-launch profiles.

use crate::dto::ProfileView;
use crate::operations::profiles;
use crate::operations::{NoView, Render};

fn row(profile: &ProfileView) -> String {
    format!(
        "{:<20} {:<11} {:<10} {:<8} {}",
        profile.name,
        profile.class,
        profile.agent_kind,
        if profile.strict { "strict" } else { "mutable" },
        profile.description
    )
}

impl Render for profiles::list::Op {
    fn text(output: &Vec<ProfileView>, _: &NoView) -> String {
        if output.is_empty() {
            return "(no profiles)".to_string();
        }
        output.iter().map(row).collect::<Vec<_>>().join("\n")
    }
}
