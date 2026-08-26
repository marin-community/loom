//! Helpers shared by more than one CLI command group.

use crate::client;
use anyhow::Result;

pub fn configure_agent_client() -> Result<()> {
    crate::cli::agent::set_client_override(client::default()?)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_the_max_length() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a very long string", 6), "a ver…");
    }
}
