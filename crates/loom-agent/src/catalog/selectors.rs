//! One vocabulary for every ACP harness's live config options.
//!
//! The picker and the launch path both call this. Categories are stable
//! across adapters; option ids and value spellings are not, so matching is
//! by category and ids are used only to tell the thinking toggle apart from
//! the effort scale.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selector {
    Model,
    Effort,
    Fast,
    Thinking,
    Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedSelectors {
    pub effort: Vec<Choice>,
    pub fast: Vec<Choice>,
    pub thinking: Vec<Choice>,
    pub mode: Vec<Choice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedOption {
    pub id: String,
    /// Canonical current value (`true`/`false` for fast and thinking).
    pub current: Option<String>,
}

const TRUE_ALIASES: &[&str] = &["true", "on", "1", "yes"];
const FALSE_ALIASES: &[&str] = &["false", "off", "0", "no"];

/// Cursor opts into separate per-model options. Without this, the adapter
/// bundles effort and fast into the model id and no selector appears.
pub fn initialize_meta(harness: &str) -> Option<Value> {
    (harness == "cursor-agent").then(|| serde_json::json!({ "parameterizedModelPicker": true }))
}

pub fn classify(option: &Value) -> Option<Selector> {
    let id = option_id(option);
    match option.get("category").and_then(Value::as_str) {
        Some("model") => Some(Selector::Model),
        Some("thought_level") if id == "thinking" => Some(Selector::Thinking),
        Some("thought_level") => Some(Selector::Effort),
        Some("model_config") if is_fast_id(id) => Some(Selector::Fast),
        Some("mode") => Some(Selector::Mode),
        _ => None,
    }
}

fn is_fast_id(id: &str) -> bool {
    id == "fast" || id == "fast-mode" || id.contains("fast")
}

pub fn normalize(options: &[Value]) -> NormalizedSelectors {
    let mut out = NormalizedSelectors::default();
    for option in options {
        let Some(kind) = classify(option) else {
            continue;
        };
        let choices = option_choices(option, kind);
        match kind {
            Selector::Effort => out.effort = choices,
            Selector::Fast => out.fast = choices,
            Selector::Thinking => out.thinking = choices,
            Selector::Mode => out.mode = choices,
            Selector::Model => {}
        }
    }
    out
}

pub fn find(options: &[Value], kind: Selector) -> Option<MatchedOption> {
    options.iter().find_map(|option| {
        (classify(option) == Some(kind)).then(|| MatchedOption {
            id: option_id(option).to_string(),
            current: current_canonical(option, kind),
        })
    })
}

/// The value to send to `session/set_config_option`.
///
/// Model and effort ids are forwarded as written so the adapter can reject an
/// unknown one. Fast and thinking prefer an advertised option value, then the
/// boolean alias table. A spelling this option does not advertise is skipped.
pub fn raw_value(options: &[Value], kind: Selector, canonical: &str) -> Option<Value> {
    let option = options
        .iter()
        .find(|option| classify(option) == Some(kind))?;
    match kind {
        Selector::Fast | Selector::Thinking => boolean_raw(option, canonical),
        Selector::Model | Selector::Effort | Selector::Mode => {
            Some(Value::String(canonical.to_string()))
        }
    }
}

pub fn model_choices(options: &[Value]) -> Vec<Choice> {
    options
        .iter()
        .find(|option| classify(option) == Some(Selector::Model))
        .map(|option| option_choices(option, Selector::Model))
        .unwrap_or_default()
}

pub fn model_option_id(options: &[Value]) -> Option<&str> {
    options
        .iter()
        .find(|option| classify(option) == Some(Selector::Model))
        .map(option_id)
        .filter(|id| !id.is_empty())
}

pub fn current_model(options: &[Value]) -> Option<String> {
    find(options, Selector::Model).and_then(|matched| matched.current)
}

fn option_id(option: &Value) -> &str {
    option.get("id").and_then(Value::as_str).unwrap_or("")
}

fn option_choices(option: &Value, kind: Selector) -> Vec<Choice> {
    let mut choices = Vec::new();
    if let Some(entries) = option.get("options").and_then(Value::as_array) {
        for entry in entries {
            let Some((raw, label)) = entry_value(entry) else {
                continue;
            };
            let id = match kind {
                Selector::Fast | Selector::Thinking => {
                    canonical_bool(&raw).map(str::to_string).unwrap_or(raw)
                }
                _ => raw,
            };
            if choices.iter().any(|choice: &Choice| choice.id == id) {
                continue;
            }
            choices.push(Choice { id, label });
        }
    } else if matches!(kind, Selector::Fast | Selector::Thinking)
        && option.get("type").and_then(Value::as_str) == Some("boolean")
    {
        choices.push(Choice {
            id: "true".to_string(),
            label: "On".to_string(),
        });
        choices.push(Choice {
            id: "false".to_string(),
            label: "Off".to_string(),
        });
    }
    choices
}

fn entry_value(entry: &Value) -> Option<(String, String)> {
    let value = entry.get("value")?;
    let raw = json_scalar_string(value)?;
    let label = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&raw)
        .to_string();
    Some((raw, label))
}

fn json_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn current_canonical(option: &Value, kind: Selector) -> Option<String> {
    let current = option.get("currentValue")?;
    let raw = json_scalar_string(current)?;
    match kind {
        Selector::Fast | Selector::Thinking => {
            Some(canonical_bool(&raw).unwrap_or(raw.as_str()).to_string())
        }
        _ => Some(raw),
    }
}

fn canonical_bool(raw: &str) -> Option<&'static str> {
    if TRUE_ALIASES
        .iter()
        .any(|alias| raw.eq_ignore_ascii_case(alias))
    {
        Some("true")
    } else if FALSE_ALIASES
        .iter()
        .any(|alias| raw.eq_ignore_ascii_case(alias))
    {
        Some("false")
    } else {
        None
    }
}

fn boolean_raw(option: &Value, canonical: &str) -> Option<Value> {
    if option.get("type").and_then(Value::as_str) == Some("boolean")
        && option.get("options").is_none()
    {
        let want = canonical_bool(canonical)?;
        return Some(Value::Bool(want == "true"));
    }
    let entries = option.get("options").and_then(Value::as_array)?;
    for entry in entries {
        let Some(value) = entry.get("value") else {
            continue;
        };
        let Some(raw) = json_scalar_string(value) else {
            continue;
        };
        if raw.eq_ignore_ascii_case(canonical) {
            return Some(value.clone());
        }
    }
    let want = canonical_bool(canonical)?;
    for entry in entries {
        let Some(value) = entry.get("value") else {
            continue;
        };
        let Some(raw) = json_scalar_string(value) else {
            continue;
        };
        if canonical_bool(&raw) == Some(want) {
            return Some(value.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options(value: Value) -> Vec<Value> {
        value.as_array().cloned().unwrap()
    }

    #[test]
    fn cursor_effort_id_varies_and_thinking_stays_separate() {
        let effort_named = options(json!([
            {"id":"model","category":"model","options":[{"value":"grok","name":"Grok"}]},
            {"id":"effort","category":"thought_level","options":[
                {"value":"low","name":"Low"},{"value":"high","name":"High"}
            ]},
            {"id":"thinking","category":"thought_level","options":[
                {"value":"true","name":"On"},{"value":"false","name":"Off"}
            ]},
            {"id":"fast","category":"model_config","options":[
                {"value":"true","name":"Fast"},{"value":"false","name":"Standard"}
            ]},
            {"id":"context","category":"model_config","options":[
                {"value":"300k","name":"300k"},{"value":"1m","name":"1m"}
            ]},
            {"id":"mode","category":"mode","options":[{"value":"agent","name":"Agent"}]}
        ]));
        let normalized = normalize(&effort_named);
        assert_eq!(
            normalized.effort,
            vec![
                Choice {
                    id: "low".into(),
                    label: "Low".into()
                },
                Choice {
                    id: "high".into(),
                    label: "High".into()
                },
            ]
        );
        assert_eq!(normalized.thinking[0].id, "true");
        assert_eq!(normalized.fast[0].id, "true");
        assert!(normalized.mode.iter().any(|choice| choice.id == "agent"));
        assert!(!normalized.effort.iter().any(|choice| choice.id == "300k"));

        let reasoning = options(json!([
            {"id":"reasoning_effort","category":"thought_level","options":[
                {"value":"medium","name":"Medium"}
            ]}
        ]));
        assert_eq!(normalize(&reasoning).effort[0].id, "medium");
        assert!(normalize(&reasoning).thinking.is_empty());
    }

    #[test]
    fn claude_on_off_and_codex_fast_mode_share_canonical_booleans() {
        let claude = options(json!([
            {"id":"fast","category":"model_config","type":"select","currentValue":"off","options":[
                {"value":"on","name":"On"},{"value":"off","name":"Off"}
            ]}
        ]));
        assert_eq!(normalize(&claude).fast[0].id, "true");
        assert_eq!(normalize(&claude).fast[1].id, "false");
        assert_eq!(
            find(&claude, Selector::Fast).unwrap().current.as_deref(),
            Some("false")
        );
        assert_eq!(
            raw_value(&claude, Selector::Fast, "true"),
            Some(json!("on"))
        );

        let codex = options(json!([
            {"id":"fast-mode","category":"model_config","type":"boolean"}
        ]));
        assert_eq!(normalize(&codex).fast.len(), 2);
        assert_eq!(
            raw_value(&codex, Selector::Fast, "false"),
            Some(Value::Bool(false))
        );

        let spelled = options(json!([
            {"id":"fast-mode","category":"model_config","options":[
                {"value":true,"name":"Fast"},{"value":false,"name":"Normal"}
            ]}
        ]));
        assert_eq!(
            raw_value(&spelled, Selector::Fast, "true"),
            Some(Value::Bool(true))
        );
    }

    #[test]
    fn absence_is_empty_and_unknown_boolean_spellings_are_not_sent() {
        let bare = options(json!([
            {"id":"model","category":"model","options":[{"value":"opus","name":"Opus"}]}
        ]));
        let normalized = normalize(&bare);
        assert!(normalized.effort.is_empty());
        assert!(normalized.fast.is_empty());
        assert!(normalized.thinking.is_empty());
        assert!(raw_value(&bare, Selector::Effort, "high").is_none());

        let odd = options(json!([
            {"id":"fast","category":"model_config","options":[
                {"value":"turbo","name":"Turbo"},{"value":"normal","name":"Normal"}
            ]}
        ]));
        assert!(raw_value(&odd, Selector::Fast, "true").is_none());
        assert_eq!(
            raw_value(&odd, Selector::Fast, "turbo"),
            Some(json!("turbo"))
        );
        assert_eq!(
            raw_value(&bare, Selector::Model, "missing"),
            Some(json!("missing"))
        );
    }
}
