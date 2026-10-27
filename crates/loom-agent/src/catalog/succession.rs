//! When a saved model id is no longer listed, follow the newest listed id in
//! the same family. The family is the provider plus the name with its version
//! removed. Words after the version (`flash`, `mini`, `luna`) stay attached,
//! so a line does not jump class.

use anyhow::Result;

use crate::db::{now_iso, Db};

/// The listed id that should replace `saved`, or `None` when `saved` is still
/// listed, has no newer sibling, or cannot be placed in a family.
pub(crate) fn successor(saved: &str, listed: &[&str]) -> Option<String> {
    let saved = saved.trim();
    if saved.is_empty() || listed.contains(&saved) {
        return None;
    }
    let saved_id = versioned(saved)?;
    let mut best: Option<(&str, Vec<u64>)> = None;
    for id in listed {
        let Some(candidate) = versioned(id) else {
            continue;
        };
        if candidate.provider != saved_id.provider
            || candidate.family != saved_id.family
            || candidate.qualifiers != saved_id.qualifiers
        {
            continue;
        }
        if cmp_version(&candidate.version, &saved_id.version).is_lt() {
            continue;
        }
        let replace = match &best {
            None => true,
            Some((current, version)) => {
                let order = cmp_version(&candidate.version, version);
                order.is_gt() || (order.is_eq() && *id < *current)
            }
        };
        if replace {
            best = Some((id, candidate.version));
        }
    }
    best.map(|(id, _)| id.to_string()).filter(|id| id != saved)
}

/// Point profiles at a newer listed id after a reconcile drops the one they saved.
pub(crate) async fn upgrade_profiles(db: &Db, harness: &str, listed: &[String]) -> Result<()> {
    if listed.is_empty() {
        return Ok(());
    }
    let listed_refs: Vec<&str> = listed.iter().map(String::as_str).collect();
    let rows = sqlx::query_as::<_, (String, String)>(
        "SELECT name, model FROM profiles WHERE agent_kind = ? AND retired = 0 AND model != ''",
    )
    .bind(harness)
    .fetch_all(db)
    .await?;
    let now = now_iso();
    for (name, model) in rows {
        let Some(next) = successor(&model, &listed_refs) else {
            continue;
        };
        let updated = sqlx::query(
            "UPDATE profiles SET model = ?, revision = revision + 1, updated_at = ?
             WHERE name = ? AND model = ?",
        )
        .bind(&next)
        .bind(&now)
        .bind(&name)
        .bind(&model)
        .execute(db)
        .await?;
        if updated.rows_affected() > 0 {
            tracing::info!(
                profile = %name,
                harness,
                from = %model,
                to = %next,
                "profile model followed a newer id in the same family"
            );
        }
    }
    Ok(())
}

struct Identity {
    provider: String,
    family: String,
    version: Vec<u64>,
    qualifiers: Vec<String>,
}

enum Parsed {
    /// Brackets or a date-shaped component. Leave the saved id alone.
    Skip,
    /// No version to follow, so the id only matches itself.
    Fixed,
    Versioned(Identity),
}

fn versioned(id: &str) -> Option<Identity> {
    match parse(id) {
        Parsed::Versioned(identity) => Some(identity),
        Parsed::Skip | Parsed::Fixed => None,
    }
}

fn parse(id: &str) -> Parsed {
    if id.contains('[') || id.contains(']') {
        return Parsed::Skip;
    }
    let (provider, rest) = match id.split_once('/') {
        Some((provider, rest)) if !provider.is_empty() && !rest.is_empty() => {
            (provider.to_string(), rest)
        }
        Some(_) => return Parsed::Skip,
        None => (String::new(), id),
    };
    let mut parts = Vec::new();
    for token in rest.split('-') {
        if token.is_empty() {
            return Parsed::Skip;
        }
        match classify(token) {
            Class::Date => return Parsed::Skip,
            Class::Version(version) => parts.push(Part::Version(version)),
            Class::Glued(prefix, version) => {
                parts.push(Part::Word(prefix));
                parts.push(Part::Version(version));
            }
            Class::Word(word) => parts.push(Part::Word(word)),
        }
    }
    let Some(start) = parts
        .iter()
        .rposition(|part| matches!(part, Part::Version(_)))
    else {
        return Parsed::Fixed;
    };
    let end = parts[start..]
        .iter()
        .position(|part| !matches!(part, Part::Version(_)))
        .map_or(parts.len(), |offset| start + offset);
    let run_start = parts[..start]
        .iter()
        .rposition(|part| !matches!(part, Part::Version(_)))
        .map_or(0, |index| index + 1);
    let mut version = Vec::new();
    for part in &parts[run_start..end] {
        if let Part::Version(components) = part {
            version.extend(components);
        }
    }
    if version.is_empty() {
        return Parsed::Fixed;
    }
    let family = parts[..run_start]
        .iter()
        .map(part_label)
        .collect::<Vec<_>>()
        .join("-");
    if family.is_empty() {
        return Parsed::Fixed;
    }
    let qualifiers = parts[end..].iter().map(part_label).collect();
    Parsed::Versioned(Identity {
        provider,
        family,
        version,
        qualifiers,
    })
}

enum Part {
    Word(String),
    Version(Vec<u64>),
}

fn part_label(part: &Part) -> String {
    match part {
        Part::Word(word) => word.clone(),
        Part::Version(components) => components
            .iter()
            .map(|component| component.to_string())
            .collect::<Vec<_>>()
            .join("."),
    }
}

enum Class {
    Word(String),
    Version(Vec<u64>),
    Glued(String, Vec<u64>),
    Date,
}

fn classify(token: &str) -> Class {
    let numeric =
        token.starts_with('v') || token.chars().next().is_some_and(|c| c.is_ascii_digit());
    if numeric {
        let body = token.strip_prefix('v').unwrap_or(token);
        if let Some(version) = dotted_version(body) {
            return match version {
                None => Class::Date,
                Some(version) => Class::Version(version),
            };
        }
    }
    if let Some(split) = token.find(|c: char| c.is_ascii_digit()) {
        if split > 0 && token[..split].chars().all(|c| c.is_ascii_alphabetic()) {
            return match dotted_version(&token[split..]) {
                Some(None) => Class::Date,
                Some(Some(version)) => Class::Glued(token[..split].to_string(), version),
                None => Class::Word(token.to_string()),
            };
        }
    }
    Class::Word(token.to_string())
}

/// `Some(None)` is a date-shaped component. `Some(Some(_))` is a version.
/// `None` is not a dotted number.
fn dotted_version(token: &str) -> Option<Option<Vec<u64>>> {
    if token.is_empty() || !token.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let mut parts = Vec::new();
    for part in token.split('.') {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        if part.len() >= 8 {
            return Some(None);
        }
        parts.push(part.parse().ok()?);
    }
    Some(Some(parts))
}

fn cmp_version(left: &[u64], right: &[u64]) -> std::cmp::Ordering {
    let len = left.len().max(right.len());
    for index in 0..len {
        let order = left
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&right.get(index).copied().unwrap_or(0));
        if !order.is_eq() {
            return order;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(saved: &str, listed: &[&str]) -> Option<String> {
        successor(saved, listed)
    }

    #[test]
    fn a_removed_model_follows_the_newest_id_in_its_family() {
        let opus = [
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-opus-5-5",
            "claude-opus-6",
            "claude-sonnet-5-5",
        ];
        assert_eq!(next("claude-opus-5-5", &opus), None);
        assert_eq!(
            next("claude-opus-4-8", &opus[1..]),
            Some("claude-opus-6".to_string())
        );
        assert_eq!(next("claude-opus-7", &opus), None);
        assert_eq!(
            next("claude-opus-5.5", &["claude-opus-5-5"]),
            Some("claude-opus-5-5".to_string())
        );

        let glm = [
            "lunaroute/glm-5.2",
            "lunaroute/glm-5.4",
            "lunaroute/glm-5.3-flash",
            "opencode/glm-5.4",
            "lunaroute/glm-5p3",
        ];
        assert_eq!(
            next("lunaroute/glm-5.3", &glm),
            Some("lunaroute/glm-5.4".to_string())
        );
        assert_eq!(next("lunaroute/glm-5p3", &glm), None);
        assert_eq!(next("opus", &opus), None);
        assert_eq!(
            next(
                "claude-fable-5-1[1m]",
                &["claude-fable-5-1", "claude-fable-6"]
            ),
            None
        );
        assert_eq!(next("claude-opus-4-6-20250929", &opus), None);

        let gpt = [
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.5",
            "gpt-5.6-luna",
            "gpt-6-luna",
            "gpt-5.6-sol",
        ];
        assert_eq!(next("gpt-5.4-mini", &gpt), None);
        assert_eq!(next("gpt-5.6-luna", &gpt), None);
        assert_eq!(
            next(
                "gpt-5.6-luna",
                &["gpt-5.4", "gpt-5.5", "gpt-6-luna", "gpt-5.6-sol"]
            ),
            Some("gpt-6-luna".to_string())
        );
        assert_eq!(next("gpt-5.4", &gpt[1..]), Some("gpt-5.5".to_string()));

        let gemini = [
            "gemini-3-flash",
            "gemini-3.8-flash",
            "gemini-3.1-pro",
            "gemini-3.5-flash-lite",
        ];
        assert_eq!(
            next("gemini-2.5-flash", &gemini),
            Some("gemini-3.8-flash".to_string())
        );
        assert_eq!(
            next("kimi-k2.7-code", &["kimi-k2.5", "kimi-k3", "kimi-k2.6"]),
            None
        );
        assert_eq!(
            next("kimi-k2.5", &["kimi-k3", "kimi-k2.7-code"]),
            Some("kimi-k3".to_string())
        );
    }
}
