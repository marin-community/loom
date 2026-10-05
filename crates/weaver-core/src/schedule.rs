//! Calendar arithmetic and shared scheduling defaults.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, TimeZone, Utc};
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::watch::Trigger;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTarget {
    pub profile: String,
    pub repo: String,
    pub prompt: String,
    #[serde(default)]
    pub slack_channels: Vec<String>,
}

impl AgentTarget {
    pub fn validate(&self) -> Result<()> {
        if self.profile.trim().is_empty()
            || self.prompt.trim().is_empty()
            || self.prompt.len() > 64 * 1024
        {
            bail!("agent profile and a prompt of 1-65536 bytes are required");
        }
        let Some((owner, repo)) = self.repo.split_once('/') else {
            bail!("agent repo must be owner/name");
        };
        if owner.is_empty()
            || repo.is_empty()
            || !self
                .repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
            || repo.contains('/')
        {
            bail!("agent repo must be owner/name");
        }
        if self.slack_channels.len() > 32 || self.slack_channels.iter().any(|c| !valid_channel(c)) {
            bail!("slack_channels must contain at most 32 Slack channel IDs");
        }
        Ok(())
    }
}

pub fn valid_channel(channel: &str) -> bool {
    channel.len() >= 2
        && channel.len() <= 64
        && channel.starts_with(['C', 'G'])
        && channel.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub const RUN_TIMEOUT_SECS: i64 = 300;
pub const ARCHIVE_DELAY_SECS: i64 = 300;
pub const LATE_GRACE_SECS: i64 = 600;

pub fn interval(spec: &str) -> Result<Duration> {
    let split = spec
        .find(|c: char| !c.is_ascii_digit())
        .context("interval requires s, m or h")?;
    let (amount, unit) = spec.split_at(split);
    let n: i64 = amount.parse().context("invalid interval")?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => bail!("interval requires s, m or h"),
    };
    let seconds = n.checked_mul(multiplier).context("interval is too large")?;
    if !(1..=366 * 24 * 3600).contains(&seconds) {
        bail!("interval must be positive and at most 366 days");
    }
    Ok(Duration::seconds(seconds))
}

pub fn validate(trigger: &Trigger) -> Result<()> {
    if trigger.cron.is_some() && trigger.every.is_some() {
        bail!("choose cron or every, not both");
    }
    trigger
        .timezone
        .as_deref()
        .unwrap_or("UTC")
        .parse::<Tz>()
        .context("unknown time zone")?;
    if let Some(expr) = &trigger.cron {
        if expr.split_whitespace().count() != 5 {
            bail!("cron must have five fields");
        }
        croner::Cron::from_str(expr).context("invalid cron")?;
    }
    if let Some(every) = &trigger.every {
        interval(every)?;
    }
    Ok(())
}

/// Calculate in local wall time, resolving a repeated time to its first instant.
/// Advancing the wall-clock cursor also skips spring-forward gaps.
pub fn next(trigger: &Trigger, from: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    validate(trigger)?;
    if let Some(spec) = &trigger.every {
        return Ok(Some(
            from.checked_add_signed(interval(spec)?)
                .context("schedule exceeds calendar range")?,
        ));
    }
    let Some(expr) = &trigger.cron else {
        return Ok(None);
    };
    let zone: Tz = trigger.timezone.as_deref().unwrap_or("UTC").parse()?;
    let cron = croner::Cron::from_str(expr)?;
    let mut cursor = from.with_timezone(&zone).naive_local().and_utc();
    loop {
        cursor = cron.find_next_occurrence(&cursor, false)?;
        if let Some(local) = zone.from_local_datetime(&cursor.naive_utc()).earliest() {
            let utc = local.with_timezone(&Utc);
            if utc > from {
                return Ok(Some(utc));
            }
        }
    }
}

/// Coalesce overdue work without moving an interval's original cadence.
pub fn latest_due(
    trigger: &Trigger,
    due: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>> {
    if let Some(spec) = &trigger.every {
        let step = interval(spec)?.num_seconds();
        let elapsed = (now - due).num_seconds().max(0);
        return due
            .checked_add_signed(Duration::seconds(elapsed / step * step))
            .context("schedule exceeds calendar range");
    }
    let zone: Tz = trigger.timezone.as_deref().unwrap_or("UTC").parse()?;
    let cron = croner::Cron::from_str(trigger.cron.as_deref().context("missing cron")?)?;
    let local_now = now.with_timezone(&zone).naive_local();
    let mut cursor = local_now.and_utc();
    if let chrono::LocalResult::Ambiguous(first, second) = zone.from_local_datetime(&local_now) {
        if second.with_timezone(&Utc) <= now {
            cursor += second.signed_duration_since(first);
        }
    }
    let mut inclusive = true;
    loop {
        cursor = cron.find_previous_occurrence(&cursor, inclusive)?;
        inclusive = false;
        if let Some(local) = zone.from_local_datetime(&cursor.naive_utc()).earliest() {
            let utc = local.with_timezone(&Utc);
            if utc <= now {
                return Ok(utc.max(due));
            }
        }
    }
}

pub fn iso(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn coalescing_during_repeated_hour_selects_latest_real_occurrence() {
        let trigger = Trigger {
            cron: Some("* * * * *".into()),
            timezone: Some("America/Los_Angeles".into()),
            ..Default::default()
        };
        let due = DateTime::parse_from_rfc3339("2026-11-01T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = DateTime::parse_from_rfc3339("2026-11-01T09:10:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            iso(latest_due(&trigger, due, now).unwrap()),
            "2026-11-01T08:59:00.000Z"
        );
    }
    #[test]
    fn pacific_cron_skips_gap_and_repeated_second_instant() {
        let trigger = Trigger {
            cron: Some("30 2 * * *".into()),
            timezone: Some("America/Los_Angeles".into()),
            ..Default::default()
        };
        assert_eq!(
            next(&trigger, time("2026-03-08T08:00:00Z")).unwrap(),
            Some(time("2026-03-09T09:30:00Z"))
        );
        let trigger = Trigger {
            cron: Some("30 1 * * *".into()),
            ..trigger
        };
        assert_eq!(
            next(&trigger, time("2026-11-01T08:30:00Z")).unwrap(),
            Some(time("2026-11-02T09:30:00Z"))
        );
    }

    #[test]
    fn overdue_interval_keeps_original_phase() {
        let trigger = Trigger {
            every: Some("5m".into()),
            ..Default::default()
        };
        let latest = latest_due(
            &trigger,
            time("2026-10-05T10:00:00Z"),
            time("2026-10-05T10:17:42Z"),
        )
        .unwrap();
        assert_eq!(latest, time("2026-10-05T10:15:00Z"));
        assert_eq!(
            next(&trigger, latest).unwrap(),
            Some(time("2026-10-05T10:20:00Z"))
        );
        assert!(interval("0s").is_err());
        assert!(interval("999999999999999999h").is_err());
    }
}
