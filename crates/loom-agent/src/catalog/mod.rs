//! The uniform ACP model catalogue: one store, one probe, one reconcile.

mod probe;
mod reconcile;
pub mod selectors;
mod store;
mod succession;

pub(crate) use probe::client_capabilities;
pub use probe::LiveSession;
pub use reconcile::{spawn_refresh, write_through, Trigger};
pub use selectors::{normalize, NormalizedSelectors};
pub(crate) use succession::successor;

use crate::agent::AgentChoice;
use crate::db::Db;

/// The model a profile-saved id should launch as. When the catalogue lists
/// this harness and that id is gone, this is the newest listed id in the same
/// family. An empty catalogue leaves the id as given. Call this only for a
/// model saved on the profile.
pub async fn model_successor(db: &Db, harness: &str, model: &str) -> String {
    if model.is_empty() {
        return String::new();
    }
    let Ok(rows) = store::list(db, harness).await else {
        return model.to_string();
    };
    if rows.is_empty() {
        return model.to_string();
    }
    let listed: Vec<&str> = rows.iter().map(|row| row.model_id.as_str()).collect();
    successor(model, &listed).unwrap_or_else(|| model.to_string())
}

/// Replace a builtin's code-shipped model list with the stored catalogue.
///
/// An empty or unreadable store leaves the code-shipped lists in place, so a
/// harness that cannot accept a typed id still has its static choices until a
/// reconcile lands. Stored rows replace both lists.
pub async fn apply_stored(
    db: &Db,
    kind: &str,
    models: &mut Vec<AgentChoice>,
    efforts: &mut Vec<AgentChoice>,
) {
    let Ok(rows) = store::list(db, kind).await else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    efforts.clear();
    *models = rows
        .into_iter()
        .map(|row| {
            let normalized = row
                .options()
                .map(|options| normalize(&options))
                .unwrap_or_default();
            let label = if row.label.is_empty() {
                row.model_id.clone()
            } else {
                row.label.clone()
            };
            AgentChoice {
                id: row.model_id,
                label,
                efforts: normalized
                    .effort
                    .into_iter()
                    .map(|choice| AgentChoice::leaf(choice.id, choice.label))
                    .collect(),
                fast: normalized
                    .fast
                    .into_iter()
                    .map(|choice| AgentChoice::leaf(choice.id, choice.label))
                    .collect(),
            }
        })
        .collect();
}

pub async fn run(db: Db) {
    reconcile::run(db).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_saved_model_follows_a_listed_sibling() {
        let db = crate::db::connect_in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO harness_model_catalog (harness, model_id, label) VALUES
             ('claude', 'claude-opus-5-5', 'Opus 5.5'),
             ('claude', 'claude-sonnet-5-5', 'Sonnet 5.5')",
        )
        .execute(&db)
        .await
        .unwrap();
        // The saved id is gone; the newest listed id in its family takes over.
        assert_eq!(
            model_successor(&db, "claude", "claude-opus-4-8").await,
            "claude-opus-5-5"
        );
        // Still listed, so nothing to follow.
        assert_eq!(
            model_successor(&db, "claude", "claude-opus-5-5").await,
            "claude-opus-5-5"
        );
        // No rows at all, so the saved id is left alone.
        assert_eq!(model_successor(&db, "codex", "gpt-5.4").await, "gpt-5.4");
    }

    #[tokio::test]
    async fn an_empty_store_keeps_the_static_choices() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let mut models = vec![AgentChoice::leaf("gpt-5.5", "GPT-5.5")];
        let mut efforts = vec![AgentChoice::leaf("high", "High")];
        apply_stored(&db, "codex", &mut models, &mut efforts).await;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.5");
        assert_eq!(efforts.len(), 1);
        assert_eq!(efforts[0].id, "high");
    }

    #[tokio::test]
    async fn a_launch_does_not_invent_a_catalogue() {
        let db = crate::db::connect_in_memory().await.unwrap();
        let options = serde_json::json!([
            { "id": "model", "category": "model", "currentValue": "gpt-5.5" }
        ]);
        write_through(&db, "codex", "gpt-5.5", options.as_array().unwrap()).await;
        assert!(store::list(&db, "codex").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_probed_catalogue_takes_over_the_static_choices() {
        let db = crate::db::connect_in_memory().await.unwrap();
        sqlx::query(
            "INSERT INTO harness_model_catalog
                (harness, model_id, label, options_json, adapter_version, fetched_at)
             VALUES ('codex', 'gpt-5.6', 'GPT-5.6', ?, '1', '2026-01-01T00:00:00Z')",
        )
        .bind(
            serde_json::json!([
                { "id": "model", "category": "model", "options": [
                    { "value": "gpt-5.6", "name": "GPT-5.6" }
                ] },
                { "id": "reasoning", "category": "thought_level", "options": [
                    { "value": "high", "name": "High" }
                ] }
            ])
            .to_string(),
        )
        .execute(&db)
        .await
        .unwrap();

        let mut models = vec![AgentChoice::leaf("gpt-5.5", "GPT-5.5")];
        let mut efforts = vec![AgentChoice::leaf("low", "Low")];
        apply_stored(&db, "codex", &mut models, &mut efforts).await;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-5.6");
        assert_eq!(models[0].efforts[0].id, "high");
        // The stored per-model efforts replace the global list.
        assert!(efforts.is_empty());

        write_through(
            &db,
            "codex",
            "gpt-5.6",
            serde_json::json!([
                { "id": "model", "category": "model", "currentValue": "gpt-5.6" }
            ])
            .as_array()
            .unwrap(),
        )
        .await;
        let rows = store::list(&db, "codex").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].options_json.is_some());
    }
}
