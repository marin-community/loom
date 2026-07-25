//! Staged artifact reviews round-tripped through the real REST server.

use reqwest::StatusCode;
use serde_json::{json, Value};
use serial_test::serial;
use std::time::Duration;
use weaver_api::{
    AddReviewCommentReq, ArtifactUpsertReq, CreateReq, CreateReviewReq, SubmitReviewReq,
    UpdateReviewCommentReq,
};
use weaver_core::events::Event;

use super::fixtures::TestServer;

async fn seeded_review_target(ts: &TestServer) -> weaver_api::SessionView {
    let session = ts
        .client
        .create_session(&CreateReq {
            cwd: ts.cwd(),
            goal: Some("review an artifact".to_string()),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    ts.client
        .write_branch_artifact(
            &session.branch.id,
            "design",
            &ArtifactUpsertReq {
                content: "# Design\n\nAlpha beta gamma.\n".to_string(),
                title: Some("Design".to_string()),
                kind: Some("markdown".to_string()),
                author: Some("agent".to_string()),
                repo: false,
            },
        )
        .await
        .unwrap();
    session
}

fn new_review(session: &weaver_api::SessionView) -> CreateReviewReq {
    CreateReviewReq {
        session_id: Some(session.id.clone()),
        subject_kind: "artifact".to_string(),
        subject_key: "design".to_string(),
        subject_version: "1".to_string(),
    }
}

fn comment(body: &str) -> AddReviewCommentReq {
    AddReviewCommentReq {
        subject_version: "1".to_string(),
        anchor_kind: "text".to_string(),
        anchor: json!({
            "quote": "beta",
            "prefix": "Alpha ",
            "suffix": " gamma",
            "block_index": 1,
        }),
        body: body.to_string(),
    }
}

async fn next_delivery_event(receiver: &mut tokio::sync::broadcast::Receiver<Event>) -> Event {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), receiver.recv())
            .await
            .expect("review delivery event timed out")
            .expect("review delivery event channel closed");
        if event.kind == "review_delivery" {
            return event;
        }
    }
}

async fn make_delivery_due(db: &loom::db::Db, review_id: i64) {
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET next_attempt_at = '2000-01-01T00:00:00.000Z'
         WHERE review_id = ?",
    )
    .bind(review_id)
    .execute(db)
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn review_draft_survives_reload_and_isolated_by_operator_session_and_subject() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    let added = ts
        .client
        .add_review_comment(draft.id, &comment("Tighten this claim."))
        .await
        .unwrap();

    let reloaded = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap();
    let own = reloaded
        .iter()
        .find(|review| review.id == draft.id)
        .unwrap();
    assert_eq!(own.comments.len(), 1);
    assert_eq!(own.comments[0].body, "Tighten this claim.");

    loom::auth::add_user(&ts.state.db, "bob", None, None)
        .await
        .unwrap();
    let (token, _) = loom::auth::create_token(&ts.state.db, "bob", "reviewer", None)
        .await
        .unwrap();
    let bob = weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token));
    let bob_view = bob
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap();
    assert!(
        bob_view.iter().all(|review| review.id != draft.id),
        "another operator cannot see the draft"
    );

    let other = ts
        .client
        .create_session(&CreateReq {
            cwd: ts.cwd(),
            goal: Some("another review target".to_string()),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let wrong_target = ts
        .client
        .get(&format!(
            "/api/sessions/{}/reviews?subject_kind=artifact&subject_key=design",
            other.id
        ))
        .await;
    assert!(
        wrong_target.is_err(),
        "the artifact is not visible in another session"
    );

    let updated = ts
        .client
        .update_review_comment(
            draft.id,
            added.id,
            &UpdateReviewCommentReq {
                body: Some("Use a concrete bound.".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.body, "Use a concrete bound.");
    ts.client
        .delete_review_comment(draft.id, added.id)
        .await
        .unwrap();
    let emptied = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap();
    assert!(emptied
        .iter()
        .find(|review| review.id == draft.id)
        .unwrap()
        .comments
        .is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn stale_submit_is_acknowledged_atomic_idempotent_and_structured() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    let draft = ts
        .client
        .create_branch_review(&session.branch.id, &new_review(&session))
        .await
        .unwrap();
    let added = ts
        .client
        .add_review_comment(draft.id, &comment("Explain why this is safe."))
        .await
        .unwrap();

    ts.client
        .write_branch_artifact(
            &session.branch.id,
            "design",
            &ArtifactUpsertReq {
                content: "# Design\n\nAlpha beta gamma, revised.\n".to_string(),
                title: None,
                kind: None,
                author: Some("agent".to_string()),
                repo: false,
            },
        )
        .await
        .unwrap();
    let stale = ts
        .client
        .post(
            &format!("/api/reviews/{}/submit", draft.id),
            json!({ "summary": "Overall", "acknowledge_outdated": false }),
        )
        .await
        .unwrap_err();
    assert!(
        stale.to_string().contains("outdated"),
        "stale review is rejected explicitly: {stale}"
    );

    // Re-anchor onto the latest revision before intentionally submitting the
    // remaining review context.
    ts.client
        .update_review_comment(
            draft.id,
            added.id,
            &UpdateReviewCommentReq {
                subject_version: Some("2".to_string()),
                anchor_kind: Some("text".to_string()),
                anchor: Some(json!({
                    "quote": "beta gamma, revised",
                    "prefix": "Alpha ",
                    "suffix": ".",
                    "block_index": 1,
                })),
                body: None,
            },
        )
        .await
        .unwrap();
    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                summary: "Please address this before landing.".to_string(),
                acknowledge_outdated: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.status, "submitted");
    assert!(matches!(
        submitted.delivery_state.as_str(),
        "queued" | "delivered"
    ));

    // The same API retry returns the immutable submission and creates no
    // second event/outbox row.
    let retried = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                summary: "duplicate attempt".to_string(),
                acknowledge_outdated: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(retried.id, submitted.id);
    assert_eq!(
        retried.summary, submitted.summary,
        "retry cannot rewrite submitted feedback"
    );

    let events: Vec<String> = sqlx::query_scalar(
        "SELECT data FROM events WHERE branch_id = ? AND kind = 'review_submitted'",
    )
    .bind(&session.branch.id)
    .fetch_all(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(events.len(), 1);
    let event: Value = serde_json::from_str(&events[0]).unwrap();
    assert_eq!(event["subject"]["label"], "design");
    assert_eq!(event["subject"]["revision"], "1");
    assert_eq!(event["subject"]["current_revision"], "2");
    assert_eq!(event["comments"][0]["revision"], "2");
    assert_eq!(event["comments"][0]["body"], "Explain why this is safe.");
    assert_eq!(event["summary"], "Please address this before landing.");
    let outbox: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM review_delivery_outbox WHERE review_id = ?")
            .bind(draft.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(outbox, 1);

    let immutable = reqwest::Client::new()
        .patch(format!(
            "http://{}/api/reviews/{}/comments/{}",
            ts.addr, draft.id, added.id
        ))
        .json(&json!({ "body": "too late" }))
        .send()
        .await
        .unwrap();
    assert_eq!(immutable.status(), StatusCode::CONFLICT);

    let resolved = ts
        .client
        .resolve_review_comment(draft.id, added.id, true)
        .await
        .unwrap();
    assert_eq!(resolved.status, "resolved");
    let reopened = ts
        .client
        .resolve_review_comment(draft.id, added.id, false)
        .await
        .unwrap();
    assert_eq!(reopened.status, "submitted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn terminal_delivery_failures_retry_then_publish_failed_state() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    sqlx::query("UPDATE sessions SET term_session = 'missing-review-terminal' WHERE id = ?")
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    ts.client
        .add_review_comment(draft.id, &comment("This delivery must fail."))
        .await
        .unwrap();
    let mut events = ts.state.bus.subscribe();

    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                summary: String::new(),
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "retrying");
    assert_eq!(
        next_delivery_event(&mut events).await.data["delivery_state"],
        "retrying"
    );

    make_delivery_due(&ts.state.db, draft.id).await;
    let _ = loom::review_delivery::deliver_review(&ts.state, draft.id).await;
    assert_eq!(
        next_delivery_event(&mut events).await.data["delivery_state"],
        "retrying"
    );

    make_delivery_due(&ts.state.db, draft.id).await;
    let _ = loom::review_delivery::deliver_review(&ts.state, draft.id).await;
    assert_eq!(
        next_delivery_event(&mut events).await.data["delivery_state"],
        "failed"
    );

    let row: (String, i64) = sqlx::query_as(
        "SELECT r.delivery_state, o.attempts
         FROM reviews r
         JOIN review_delivery_outbox o ON o.review_id = r.id
         WHERE r.id = ?",
    )
    .bind(draft.id)
    .fetch_one(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(row, ("failed".to_string(), 3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn acp_delivery_receipt_deduplicates_recovered_worker_attempt() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    sqlx::query("UPDATE sessions SET protocol = 'acp', pending_prompt = '' WHERE id = ?")
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    ts.client
        .add_review_comment(draft.id, &comment("Queue this once."))
        .await
        .unwrap();
    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                summary: "ACP delivery".to_string(),
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "delivered");
    let first_prompt: String =
        sqlx::query_scalar("SELECT pending_prompt FROM sessions WHERE id = ?")
            .bind(&session.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert!(first_prompt.contains(&format!("review_id: {}", draft.id)));

    sqlx::query("UPDATE reviews SET delivery_state = 'queued' WHERE id = ?")
        .bind(draft.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'queued', next_attempt_at = '2000-01-01T00:00:00.000Z'
         WHERE review_id = ?",
    )
    .bind(draft.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    loom::review_delivery::deliver_review(&ts.state, draft.id)
        .await
        .unwrap();

    let recovered_prompt: String =
        sqlx::query_scalar("SELECT pending_prompt FROM sessions WHERE id = ?")
            .bind(&session.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    let receipts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM review_prompt_deliveries WHERE delivery_key = ?")
            .bind(&submitted.delivery_key)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(recovered_prompt, first_prompt);
    assert_eq!(receipts, 1);

    let error = ts.client.retry_review_delivery(draft.id).await.unwrap_err();
    assert!(error.to_string().contains("only failed"));
}
