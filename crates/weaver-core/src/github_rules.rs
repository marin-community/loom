//! Deployment configuration for GitHub pull request automation.

use std::collections::HashSet;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub repository_id: i64,
    pub target: Target,
    pub events: Vec<Event>,
    #[serde(default)]
    pub excluded_labels: Vec<String>,
    #[serde(default)]
    pub allowed_bots: Vec<String>,
    pub run: Run,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    Profile(String),
    Federation(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub actions: Vec<String>,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub title: String,
    pub goal: String,
    pub idempotency_key: String,
    pub channel: Option<String>,
}

/// Expand only scalar values that the dispatcher has verified against GitHub.
pub fn render(template: &str, number: &str, head: &str) -> Result<String, String> {
    let mut parts = template.split('{');
    let mut result = parts.next().unwrap_or_default().to_string();
    for part in parts {
        let (field, tail) = part
            .split_once('}')
            .ok_or_else(|| format!("unclosed template field in '{template}'"))?;
        result.push_str(match field {
            "number" => number,
            "head" => head,
            _ => return Err(format!("unknown template field '{{{field}}}'")),
        });
        result.push_str(tail);
    }
    Ok(result)
}

pub fn parse(value: &str) -> Result<Vec<Rule>, String> {
    let rules: Vec<Rule> = serde_json::from_str(value).map_err(|error| error.to_string())?;
    let mut names = HashSet::new();
    for rule in &rules {
        if rule.name.trim().is_empty() || !names.insert(&rule.name) {
            return Err(format!(
                "empty or duplicate pull request rule name '{}'",
                rule.name
            ));
        }
        let target = match &rule.target {
            Target::Profile(name) | Target::Federation(name) => name,
        };
        if rule.repository_id <= 0 || target.trim().is_empty() {
            return Err(format!(
                "rule '{}': repository ID and target are required",
                rule.name
            ));
        }
        if rule.events.is_empty()
            || rule.events.iter().any(|event| {
                event.actions.is_empty() || event.actions.iter().any(|action| action.is_empty())
            })
        {
            return Err(format!(
                "rule '{}': each event must list an action",
                rule.name
            ));
        }
        for template in [&rule.run.title, &rule.run.goal, &rule.run.idempotency_key]
            .into_iter()
            .chain(rule.run.channel.iter())
        {
            if template.trim().is_empty() {
                return Err(format!(
                    "rule '{}': launch templates must not be empty",
                    rule.name
                ));
            }
            render(template, "42", "head")
                .map_err(|error| format!("rule '{}': {error}", rule.name))?;
        }
    }
    Ok(rules)
}
