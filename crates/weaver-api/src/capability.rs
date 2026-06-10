//! The capability gate — the security-relevant core of the intervention ladder.
//!
//! An out-of-process consumer (the Python binding, a scripted overlooker) is
//! constructed with a *granted* capability set and must pass each mutating
//! action through [`require`] before it touches the fleet. Keeping the gate here
//! — a pure function over `&[String]`, not buried in pyo3 glue — means the same
//! check the binding enforces is unit-tested by the workspace `test` job. The
//! capability vocabulary itself lives in `weaver_core::overlooker::CAPABILITIES`
//! (observe / mark / escalate / nudge / interrupt / launch); this module decides
//! whether a given grant set admits a given action.

use std::fmt;

pub use weaver_core::overlooker::CAPABILITIES;

/// A denied action: the capability a call needed and the set it was checked
/// against. The binding maps this to a Python exception; Rust callers match on
/// it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    /// The action needed `needed`, which is not in the granted set `granted`.
    Denied {
        needed: String,
        granted: Vec<String>,
    },
    /// `needed` is not a capability the system defines at all — a typo or a
    /// stale grant. Rejected rather than silently treated as ungranted, so a
    /// misspelled gate name can never read as "allowed".
    Unknown { needed: String },
}

impl fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CapabilityError::Denied { needed, granted } => write!(
                f,
                "capability '{needed}' is required but not granted (have: [{}])",
                granted.join(", ")
            ),
            CapabilityError::Unknown { needed } => write!(
                f,
                "'{needed}' is not a known capability (known: [{}])",
                CAPABILITIES.join(", ")
            ),
        }
    }
}

impl std::error::Error for CapabilityError {}

/// Whether `cap` is a capability the system defines.
pub fn is_known(cap: &str) -> bool {
    CAPABILITIES.contains(&cap)
}

/// Gate a mutating action: succeed iff `needed` is held by `granted`.
///
/// `observe` is implicit — always granted, the read floor — mirroring
/// `Overlooker::has_capability`. An unknown `needed` is a hard error, never an
/// implicit denial, so a typo'd gate can't read as "allowed". A `granted` set
/// containing an unknown capability is tolerated (it simply grants nothing); the
/// gate only validates the capability being *required*.
pub fn require(granted: &[String], needed: &str) -> Result<(), CapabilityError> {
    if !is_known(needed) {
        return Err(CapabilityError::Unknown {
            needed: needed.to_string(),
        });
    }
    if needed == "observe" || granted.iter().any(|c| c == needed) {
        return Ok(());
    }
    Err(CapabilityError::Denied {
        needed: needed.to_string(),
        granted: granted.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn observe_is_always_granted_even_with_an_empty_set() {
        assert!(require(&[], "observe").is_ok());
    }

    #[test]
    fn a_granted_capability_passes() {
        let granted = caps(&["observe", "mark", "nudge"]);
        assert!(require(&granted, "mark").is_ok());
        assert!(require(&granted, "nudge").is_ok());
    }

    #[test]
    fn each_mutating_capability_is_denied_when_absent() {
        // Only observe + mark granted; the louder rungs of the ladder are not.
        let granted = caps(&["observe", "mark"]);
        for needed in ["nudge", "interrupt", "launch", "escalate"] {
            let err = require(&granted, needed).unwrap_err();
            assert_eq!(
                err,
                CapabilityError::Denied {
                    needed: needed.to_string(),
                    granted: granted.clone(),
                },
                "{needed} must be denied when not granted"
            );
        }
    }

    #[test]
    fn mark_is_denied_with_only_observe() {
        let granted = caps(&["observe"]);
        assert!(matches!(
            require(&granted, "mark"),
            Err(CapabilityError::Denied { .. })
        ));
    }

    #[test]
    fn an_unknown_capability_is_a_hard_error_not_a_silent_deny() {
        // A typo'd gate name must never be treated as merely ungranted — that
        // would let a misspelled require() read as "allowed" if it were ever
        // inverted. It is rejected as Unknown regardless of the grant set.
        assert!(matches!(
            require(&caps(&["observe"]), "marrk"),
            Err(CapabilityError::Unknown { .. })
        ));
        // Even if the bogus name is literally in the granted set, requiring an
        // unknown capability is still an error.
        assert!(matches!(
            require(&caps(&["bogus"]), "bogus"),
            Err(CapabilityError::Unknown { .. })
        ));
    }

    #[test]
    fn known_covers_the_whole_ladder() {
        for c in [
            "observe",
            "mark",
            "escalate",
            "nudge",
            "interrupt",
            "launch",
        ] {
            assert!(is_known(c), "{c} is part of the ladder");
        }
        assert!(!is_known("frobnicate"));
    }

    #[test]
    fn display_names_the_missing_capability() {
        let err = require(&caps(&["observe"]), "nudge").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nudge"), "message names the needed capability");
    }
}
