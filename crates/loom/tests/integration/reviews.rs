//! Staged artifact reviews round-tripped through the real REST server and CLI.

use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{json, Value};
use serial_test::serial;
use std::time::Duration;
use tokio::process::Command;
use weaver_api::{
    AddReviewCommentReq, ArtifactTextAnchorDto, ArtifactUpsertReq, CreateReq, CreateReviewReq,
    SubmitReviewReq, UpdateReviewCommentReq, UpdateReviewReq,
};

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
    seed_artifact(ts, &session).await;
    session
}

async fn seed_artifact(ts: &TestServer, session: &weaver_api::SessionView) {
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
}

fn new_review(session: &weaver_api::SessionView) -> CreateReviewReq {
    CreateReviewReq {
        session_id: Some(session.id.clone()),
        subject_kind: "artifact".to_string(),
        subject_key: "design".to_string(),
        subject_version: "1".to_string(),
    }
}

fn comment(expected_revision: i64, body: &str) -> AddReviewCommentReq {
    AddReviewCommentReq {
        expected_revision,
        subject_version: "1".to_string(),
        anchor_kind: "text".to_string(),
        anchor: ArtifactTextAnchorDto {
            quote: "beta".to_string(),
            prefix: "Alpha ".to_string(),
            suffix: " gamma".to_string(),
            block_index: Some(1),
        },
        body: body.to_string(),
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
async fn draft_contract_is_durable_private_guarded_and_canonical() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    let mut branch_events = ts.state.bus.subscribe();
    let agent_token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        &session.id,
        &session.branch.id,
    )
    .await
    .unwrap();
    let agent_sse = reqwest::Client::new()
        .get(format!(
            "http://{}/api/sessions/{}/events",
            ts.addr, session.id
        ))
        .bearer_auth(&agent_token)
        .send()
        .await
        .unwrap();
    assert_eq!(agent_sse.status(), StatusCode::OK);
    let mut agent_sse = agent_sse.bytes_stream();
    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    assert!(draft.subject.id.parse::<i64>().unwrap() > 0);
    assert_eq!(draft.subject.key, "design");
    assert_eq!(draft.draft_revision, 1);

    let with_summary = ts
        .client
        .update_review(
            draft.id,
            &UpdateReviewReq {
                expected_revision: draft.draft_revision,
                summary: Some("Durable overall feedback".to_string()),
                subject_version: None,
            },
        )
        .await
        .unwrap();
    let added = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(with_summary.draft_revision, "Tighten this claim."),
        )
        .await
        .unwrap();
    let comment_id = added.comments[0].id;
    assert!(added.message.contains("\"prefix\":\"Alpha \""));
    assert!(added.message.contains("\"suffix\":\" gamma\""));
    assert!(added.message.contains("\"block_index\":1"));

    let reloaded = ts
        .client
        .list_session_reviews(&session.id, "artifact", &added.subject.key)
        .await
        .unwrap();
    let own = reloaded
        .iter()
        .find(|review| review.id == draft.id)
        .unwrap();
    assert_eq!(own.summary, "Durable overall feedback");
    assert_eq!(own.comments[0].body, "Tighten this claim.");

    loom::auth::add_user(&ts.state.db, "bob", None, None)
        .await
        .unwrap();
    let (token, _) = loom::auth::create_token(&ts.state.db, "bob", "reviewer", None)
        .await
        .unwrap();
    let bob = weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token));
    assert!(bob
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .iter()
        .all(|review| review.id != draft.id));

    let agent =
        weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(agent_token));
    assert!(agent
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .iter()
        .all(|review| review.id != draft.id));

    while let Ok(event) = branch_events.try_recv() {
        assert_ne!(
            event.kind, "review_draft_changed",
            "private draft existence must not leak over branch SSE"
        );
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
    let mut sse_text = String::new();
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, agent_sse.next()).await {
            Ok(Some(Ok(bytes))) => sse_text.push_str(&String::from_utf8_lossy(&bytes)),
            _ => break,
        }
    }
    assert!(!sse_text.contains("review_draft_changed"));
    assert!(!sse_text.contains("Durable overall feedback"));
    assert!(!sse_text.contains(&format!("\"review_id\":{}", draft.id)));

    let stale = reqwest::Client::new()
        .patch(format!(
            "http://{}/api/reviews/{}/comments/{}",
            ts.addr, draft.id, comment_id
        ))
        .json(&UpdateReviewCommentReq {
            expected_revision: with_summary.draft_revision,
            body: Some("unseen overwrite".to_string()),
            ..Default::default()
        })
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    let stale: Value = stale.json().await.unwrap();
    assert_eq!(
        stale["details"]["review"]["draft_revision"],
        added.draft_revision
    );

    let updated = ts
        .client
        .update_review_comment(
            draft.id,
            comment_id,
            &UpdateReviewCommentReq {
                expected_revision: added.draft_revision,
                body: Some("Use a concrete bound.".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.comments[0].body, "Use a concrete bound.");
    let emptied = ts
        .client
        .delete_review_comment(draft.id, comment_id, updated.draft_revision)
        .await
        .unwrap();
    assert!(emptied.comments.is_empty());
    assert_eq!(emptied.summary, "Durable overall feedback");

    let invalid = reqwest::Client::new()
        .post(format!(
            "http://{}/api/reviews/{}/comments",
            ts.addr, draft.id
        ))
        .json(&json!({
            "expected_revision": emptied.draft_revision,
            "subject_version": "1",
            "anchor_kind": "anything",
            "anchor": {},
            "body": "invalid"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let invalid_kind = reqwest::Client::new()
        .post(format!(
            "http://{}/api/reviews/{}/comments",
            ts.addr, draft.id
        ))
        .json(&json!({
            "expected_revision": emptied.draft_revision,
            "subject_version": "1",
            "anchor_kind": "diff-line",
            "anchor": {
                "quote": "beta",
                "prefix": "Alpha ",
                "suffix": " gamma",
                "block_index": 1
            },
            "body": "invalid kind"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_kind.status(), StatusCode::BAD_REQUEST);

    ts.client
        .discard_review(draft.id, emptied.draft_revision)
        .await
        .unwrap();
    assert!(ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .iter()
        .all(|review| review.id != draft.id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn second_operator_can_collaborate_only_after_submit() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    loom::auth::add_user(&ts.state.db, "bob", None, None)
        .await
        .unwrap();
    let (token, _) = loom::auth::create_token(&ts.state.db, "bob", "reviewer", None)
        .await
        .unwrap();
    let bob =
        weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token.clone()));

    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    let draft = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(draft.draft_revision, "Submitted collaboration"),
        )
        .await
        .unwrap();
    assert!(bob
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .is_empty());
    let hidden_mutation = reqwest::Client::new()
        .patch(format!("http://{}/api/reviews/{}", ts.addr, draft.id))
        .bearer_auth(&token)
        .json(&UpdateReviewReq {
            expected_revision: draft.draft_revision,
            summary: Some("not mine".to_string()),
            subject_version: None,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(hidden_mutation.status(), StatusCode::NOT_FOUND);

    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: draft.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert!(bob
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .iter()
        .any(|review| review.id == submitted.id));
    let comment_id = submitted.comments[0].id;
    assert_eq!(
        bob.resolve_review_comment(submitted.id, comment_id, true)
            .await
            .unwrap()
            .status,
        "resolved"
    );
    assert_eq!(
        bob.resolve_review_comment(submitted.id, comment_id, false)
            .await
            .unwrap()
            .status,
        "submitted"
    );

    sqlx::query("UPDATE sessions SET status = 'orphaned' WHERE id = ?")
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'failed', attempts = 3, lease_token = NULL
         WHERE review_id = ?",
    )
    .bind(submitted.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE reviews SET delivery_state = 'failed', delivery_error = 'transport failed'
         WHERE id = ?",
    )
    .bind(submitted.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    let retried = bob.retry_review_delivery(submitted.id).await.unwrap();
    assert_eq!(retried.delivery_state, "queued");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn immutable_subject_id_prevents_shadow_and_recreate_projection() {
    let ts = TestServer::start().await;
    let session = ts
        .client
        .create_session(&CreateReq {
            cwd: ts.cwd(),
            goal: Some("review stable identities".to_string()),
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
                content: "# Shared\n".to_string(),
                title: Some("Shared".to_string()),
                kind: Some("markdown".to_string()),
                author: Some("agent".to_string()),
                repo: true,
            },
        )
        .await
        .unwrap();
    let shared = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();

    seed_artifact(&ts, &session).await;
    let visible = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap();
    assert!(visible.iter().all(|review| review.id != shared.id));
    let scoped = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    assert_ne!(scoped.id, shared.id);
    assert_ne!(scoped.subject.id, shared.subject.id);

    let shared = ts
        .client
        .update_review(
            shared.id,
            &UpdateReviewReq {
                expected_revision: shared.draft_revision,
                summary: Some("Review the shared artifact".to_string()),
                subject_version: None,
            },
        )
        .await
        .unwrap();
    let shared = ts
        .client
        .submit_review(
            shared.id,
            &SubmitReviewReq {
                expected_revision: shared.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        ts.client.get_review(shared.id).await.unwrap().subject.id,
        shared.subject.id
    );
    assert!(ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .iter()
        .all(|review| review.id != shared.id));
    sqlx::query(
        "UPDATE review_delivery_outbox SET state = 'failed', attempts = 3 WHERE review_id = ?",
    )
    .bind(shared.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE reviews SET delivery_state = 'failed', delivery_error = 'shadowed transport'
         WHERE id = ?",
    )
    .bind(shared.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE sessions SET status = 'orphaned' WHERE id = ?")
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    assert_eq!(
        ts.client.retry_review_delivery(shared.id).await.unwrap().id,
        shared.id
    );
    sqlx::query("UPDATE sessions SET status = 'running' WHERE id = ?")
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();

    let scoped_id = scoped.subject.id.clone();
    let scoped = ts
        .client
        .update_review(
            scoped.id,
            &UpdateReviewReq {
                expected_revision: scoped.draft_revision,
                summary: Some("Review the scoped artifact".to_string()),
                subject_version: None,
            },
        )
        .await
        .unwrap();
    let scoped = ts
        .client
        .submit_review(
            scoped.id,
            &SubmitReviewReq {
                expected_revision: scoped.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    ts.client
        .delete_branch_artifact(&session.branch.id, "design", false)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE review_delivery_outbox SET state = 'failed', attempts = 3 WHERE review_id = ?",
    )
    .bind(scoped.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE reviews SET delivery_state = 'failed', delivery_error = 'transport failed'
         WHERE id = ?",
    )
    .bind(scoped.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE sessions SET status = 'orphaned' WHERE id = ?")
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let old = ts.client.get_review(scoped.id).await.unwrap();
    assert_eq!(old.subject.id, scoped_id);
    assert_eq!(
        ts.client.retry_review_delivery(scoped.id).await.unwrap().id,
        scoped.id
    );

    seed_artifact(&ts, &session).await;
    let replacement = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    assert_ne!(replacement.subject.id, scoped_id);
    assert!(ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .iter()
        .all(|review| review.id != scoped.id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn stale_reanchor_submit_event_and_preview_are_exact_and_idempotent() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    let draft = ts
        .client
        .create_branch_review(&session.branch.id, &new_review(&session))
        .await
        .unwrap();
    let added = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(draft.draft_revision, "Explain why this is safe."),
        )
        .await
        .unwrap();
    let comment_id = added.comments[0].id;

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
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: added.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap_err();
    assert!(stale.to_string().contains("outdated"));

    let reanchored = ts
        .client
        .update_review_comment(
            draft.id,
            comment_id,
            &UpdateReviewCommentReq {
                expected_revision: added.draft_revision,
                subject_version: Some("2".to_string()),
                anchor_kind: Some("text".to_string()),
                anchor: Some(ArtifactTextAnchorDto {
                    quote: "beta gamma, revised".to_string(),
                    prefix: "Alpha ".to_string(),
                    suffix: ".".to_string(),
                    block_index: Some(1),
                }),
                body: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reanchored.subject.version, "2");
    assert!(!reanchored.outdated);
    let ready = ts
        .client
        .update_review(
            draft.id,
            &UpdateReviewReq {
                expected_revision: reanchored.draft_revision,
                summary: Some("Please address this before landing.".to_string()),
                subject_version: None,
            },
        )
        .await
        .unwrap();
    let exact_preview = ready.message.clone();
    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: ready.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.status, "submitted");
    assert_eq!(submitted.message, exact_preview);

    let stale_retry = reqwest::Client::new()
        .post(format!(
            "http://{}/api/reviews/{}/submit",
            ts.addr, draft.id
        ))
        .json(&SubmitReviewReq {
            expected_revision: 0,
            acknowledge_outdated: true,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(stale_retry.status(), StatusCode::CONFLICT);
    let stale_retry: Value = stale_retry.json().await.unwrap();
    assert_eq!(
        stale_retry["details"]["review"]["draft_revision"],
        ready.draft_revision
    );

    let retried = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: ready.draft_revision,
                acknowledge_outdated: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(retried.message, exact_preview);

    let events: Vec<String> = sqlx::query_scalar(
        "SELECT data FROM events WHERE branch_id = ? AND kind = 'review_submitted'",
    )
    .bind(&session.branch.id)
    .fetch_all(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(events.len(), 1);
    let event: Value = serde_json::from_str(&events[0]).unwrap();
    assert_eq!(event["subject"]["id"], submitted.subject.id);
    assert_eq!(event["subject"]["key"], "design");
    assert_eq!(event["subject"]["revision"], "2");
    assert_eq!(event["subject"]["current_revision"], "2");
    assert_eq!(event["comments"][0]["anchor"]["prefix"], "Alpha ");
    assert_eq!(event["comments"][0]["anchor"]["suffix"], ".");
    assert_eq!(event["comments"][0]["anchor"]["block_index"], 1);
    assert_eq!(event["message"], exact_preview);

    let event_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM review_delivery_outbox WHERE review_id = ?")
            .bind(draft.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(event_count, 1);

    let immutable = reqwest::Client::new()
        .patch(format!(
            "http://{}/api/reviews/{}/comments/{}",
            ts.addr, draft.id, comment_id
        ))
        .json(&json!({
            "expected_revision": ready.draft_revision,
            "body": "too late"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(immutable.status(), StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn artifact_revision_spellings_are_canonical_at_every_write_boundary() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    let draft = ts
        .client
        .create_session_review(
            &session.id,
            &CreateReviewReq {
                subject_version: "01".to_string(),
                ..new_review(&session)
            },
        )
        .await
        .unwrap();
    assert_eq!(draft.subject.version, "1");
    assert!(!draft.outdated);
    let added = ts
        .client
        .add_review_comment(
            draft.id,
            &AddReviewCommentReq {
                subject_version: "01".to_string(),
                ..comment(draft.draft_revision, "Canonical comment")
            },
        )
        .await
        .unwrap();
    assert_eq!(added.comments[0].subject_version, "1");
    let envelope = ts
        .client
        .update_review(
            draft.id,
            &UpdateReviewReq {
                expected_revision: added.draft_revision,
                summary: Some("Canonical envelope".to_string()),
                subject_version: Some("01".to_string()),
            },
        )
        .await
        .unwrap();
    assert_eq!(envelope.subject.version, "1");

    ts.client
        .write_branch_artifact(
            &session.branch.id,
            "design",
            &ArtifactUpsertReq {
                content: "# Design\n\nAlpha beta gamma revised.\n".to_string(),
                title: None,
                kind: None,
                author: Some("agent".to_string()),
                repo: false,
            },
        )
        .await
        .unwrap();
    let reanchored = ts
        .client
        .update_review_comment(
            draft.id,
            envelope.comments[0].id,
            &UpdateReviewCommentReq {
                expected_revision: envelope.draft_revision,
                subject_version: Some("02".to_string()),
                anchor_kind: Some("text".to_string()),
                anchor: Some(ArtifactTextAnchorDto {
                    quote: "beta".to_string(),
                    prefix: "Alpha ".to_string(),
                    suffix: " gamma revised".to_string(),
                    block_index: Some(1),
                }),
                body: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reanchored.subject.version, "2");
    assert_eq!(reanchored.comments[0].subject_version, "2");
    assert!(!reanchored.outdated);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn offline_terminal_delivery_waits_without_attempts_then_recovers() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    let original_terminal: String =
        sqlx::query_scalar("SELECT term_session FROM sessions WHERE id = ?")
            .bind(&session.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    let draft = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(draft.draft_revision, "Queue while offline."),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE sessions
         SET term_session = 'missing-review-terminal', status = 'orphaned'
         WHERE id = ?",
    )
    .bind(&session.id)
    .execute(&ts.state.db)
    .await
    .unwrap();

    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: draft.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "queued");
    let attempts: i64 =
        sqlx::query_scalar("SELECT attempts FROM review_delivery_outbox WHERE review_id = ?")
            .bind(draft.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(attempts, 0);
    sqlx::query(
        "UPDATE review_delivery_outbox
         SET state = 'delivering', lease_token = 'expired-owner',
             next_attempt_at = '1970-01-01T00:00:00.000Z'
         WHERE review_id = ?",
    )
    .bind(draft.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    sqlx::query("UPDATE reviews SET delivery_state = 'delivering' WHERE id = ?")
        .bind(draft.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    loom::review_delivery::deliver_review(&ts.state, draft.id)
        .await
        .unwrap();
    let ownerless: (String, i64, Option<String>) = sqlx::query_as(
        "SELECT state, attempts, lease_token
         FROM review_delivery_outbox WHERE review_id = ?",
    )
    .bind(draft.id)
    .fetch_one(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(ownerless, ("queued".to_string(), 0, None));

    sqlx::query("UPDATE sessions SET term_session = ?, status = 'running' WHERE id = ?")
        .bind(original_terminal)
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    make_delivery_due(&ts.state.db, draft.id).await;
    loom::review_delivery::deliver_review(&ts.state, draft.id)
        .await
        .unwrap();
    let recovered = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .into_iter()
        .find(|review| review.id == draft.id)
        .unwrap();
    assert_eq!(recovered.delivery_state, "delivered");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn delivery_falls_back_to_the_newest_usable_session_on_the_branch() {
    let ts = TestServer::start().await;
    let target = seeded_review_target(&ts).await;
    let fallback = ts
        .client
        .create_session(&CreateReq {
            cwd: ts.cwd(),
            goal: Some("replacement conversation".to_string()),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    let draft = ts
        .client
        .create_session_review(&target.id, &new_review(&target))
        .await
        .unwrap();
    let draft = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(draft.draft_revision, "Route this to the live replacement."),
        )
        .await
        .unwrap();
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(&target.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET branch_id = ? WHERE id = ?")
        .bind(&target.branch.id)
        .bind(&fallback.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let mut receiver = ts.state.bus.subscribe();
    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: draft.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "delivered");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        if event.kind == "review_delivery" {
            assert_eq!(event.data["delivery_session_id"], fallback.id);
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn real_incompatible_terminal_transport_retries_then_fails() {
    let ts = TestServer::start().await;
    super::acp::start_new(&ts, "review-transport-failure", None, None).await;
    let session = ts
        .client
        .get_session("review-transport-failure")
        .await
        .unwrap();
    seed_artifact(&ts, &session).await;
    // The live Tapestry supervisor is a relay, not a PTY. Treating it as a
    // terminal keeps the liveness preflight honest but makes the real SEND
    // transport reject bracketed paste deterministically.
    sqlx::query("UPDATE sessions SET protocol = 'terminal' WHERE id = ?")
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    let draft = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(
                draft.draft_revision,
                "Exercise an actual rejected transport.",
            ),
        )
        .await
        .unwrap();
    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: draft.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "retrying");

    for expected in ["retrying", "failed"] {
        make_delivery_due(&ts.state.db, draft.id).await;
        let error = loom::review_delivery::deliver_review(&ts.state, draft.id)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("pasting review feedback"),
            "{error}"
        );
        let state: String = sqlx::query_scalar("SELECT delivery_state FROM reviews WHERE id = ?")
            .bind(draft.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
        assert_eq!(state, expected);
    }
    let attempts: i64 =
        sqlx::query_scalar("SELECT attempts FROM review_delivery_outbox WHERE review_id = ?")
            .bind(draft.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(attempts, 3);
}

async fn poll_review_turn(ts: &TestServer, session_id: &str, payload: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let chat = ts
            .client
            .get(&format!("/api/sessions/{session_id}/chat"))
            .await
            .unwrap();
        let count = chat["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["kind"] == "user_message" && block["payload"]["text"] == payload)
            .count();
        if count == 1 {
            return chat;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "protected review did not settle as one logical turn: {chat}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn acp_review_inbox_is_protected_retractable_prompt_is_separate_and_logically_deduplicated() {
    let ts = TestServer::start().await;
    super::acp::start_new(&ts, "acp-review", None, None).await;
    let session = ts.client.get_session("acp-review").await.unwrap();
    seed_artifact(&ts, &session).await;

    ts.client
        .post(
            "/api/sessions/acp-review/prompt",
            json!({ "text": "wait:1200|say:first" }),
        )
        .await
        .unwrap();
    let queued = ts
        .client
        .post(
            "/api/sessions/acp-review/prompt",
            json!({ "text": "say:editable feedback" }),
        )
        .await
        .unwrap();
    assert_eq!(queued["queued"], true);

    let draft = ts
        .client
        .create_session_review(&session.id, &new_review(&session))
        .await
        .unwrap();
    let draft = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(draft.draft_revision, "Protected immutable feedback."),
        )
        .await
        .unwrap();
    let draft = ts
        .client
        .update_review(
            draft.id,
            &UpdateReviewReq {
                expected_revision: draft.draft_revision,
                summary: Some("ACP exact delivery".to_string()),
                subject_version: None,
            },
        )
        .await
        .unwrap();
    let payload = draft.message.clone();
    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: draft.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "delivered");

    let retracted = ts
        .client
        .delete("/api/sessions/acp-review/prompt")
        .await
        .unwrap();
    assert_eq!(retracted["text"], "say:editable feedback");
    let inbox_payload: String =
        sqlx::query_scalar("SELECT payload FROM review_conversation_inbox WHERE delivery_key = ?")
            .bind(&submitted.delivery_key)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(inbox_payload, payload);
    let chat = poll_review_turn(&ts, "acp-review", &payload).await;
    assert!(chat["pending_prompt"].is_null());
    let protected = chat["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == "user_message" && block["payload"]["text"] == payload)
        .unwrap();
    assert_eq!(protected["payload"]["delivery_key"], submitted.delivery_key);
    let inbox_state: String =
        sqlx::query_scalar("SELECT state FROM review_conversation_inbox WHERE delivery_key = ?")
            .bind(&submitted.delivery_key)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(inbox_state, "consumed");

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
    tokio::time::sleep(Duration::from_millis(200)).await;
    let chat = ts
        .client
        .get("/api/sessions/acp-review/chat")
        .await
        .unwrap();
    assert_eq!(
        chat["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["kind"] == "user_message" && block["payload"]["text"] == payload)
            .count(),
        1
    );
    let inbox_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM review_conversation_inbox WHERE delivery_key = ?")
            .bind(&submitted.delivery_key)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(inbox_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn queued_acp_review_rehomes_to_a_terminal_successor() {
    let ts = TestServer::start().await;
    super::acp::start_new(&ts, "acp-review-rehome", None, None).await;
    let acp = ts.client.get_session("acp-review-rehome").await.unwrap();
    seed_artifact(&ts, &acp).await;
    ts.client
        .post(
            "/api/sessions/acp-review-rehome/prompt",
            json!({ "text": "wait:30000|say:keep-busy" }),
        )
        .await
        .unwrap();

    let draft = ts
        .client
        .create_session_review(&acp.id, &new_review(&acp))
        .await
        .unwrap();
    let draft = ts
        .client
        .add_review_comment(
            draft.id,
            &comment(draft.draft_revision, "Rehome this immutable review."),
        )
        .await
        .unwrap();
    let payload = draft.message.clone();
    let submitted = ts
        .client
        .submit_review(
            draft.id,
            &SubmitReviewReq {
                expected_revision: draft.draft_revision,
                acknowledge_outdated: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "delivered");
    let queued: String =
        sqlx::query_scalar("SELECT state FROM review_conversation_inbox WHERE delivery_key = ?")
            .bind(&submitted.delivery_key)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!(queued, "queued");

    assert!(ts.state.acp.stop(&acp.id));
    sqlx::query("UPDATE sessions SET status = 'archived' WHERE id = ?")
        .bind(&acp.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let terminal = ts
        .client
        .create_session(&CreateReq {
            cwd: ts.cwd(),
            goal: Some("terminal review successor".to_string()),
            agent: Some("shell".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET branch_id = ? WHERE id = ?")
        .bind(&acp.branch.id)
        .bind(&terminal.id)
        .execute(&ts.state.db)
        .await
        .unwrap();

    loom::review_delivery::drain(&ts.state).await.unwrap();
    let consumed: (String, String, String) = sqlx::query_as(
        "SELECT state, claimed_session_id, payload
         FROM review_conversation_inbox WHERE delivery_key = ?",
    )
    .bind(&submitted.delivery_key)
    .fetch_one(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(consumed.0, "consumed");
    assert_eq!(consumed.1, terminal.id);
    assert_eq!(consumed.2, payload);
    let acp_chat = ts
        .client
        .get("/api/sessions/acp-review-rehome/chat")
        .await
        .unwrap();
    assert_eq!(
        acp_chat["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|block| block["payload"]["text"] == payload)
            .count(),
        0
    );
}

async fn loom_review_cli(ts: &TestServer, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(args)
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .output()
        .await
        .unwrap()
}

fn assert_cli_ok(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "CLI failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn real_loom_review_commands_cover_overall_mutations_submit_and_discard() {
    let ts = TestServer::start().await;
    let session = seeded_review_target(&ts).await;
    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "overall",
                &session.id,
                "design",
                "--rev",
                "1",
                "Overall",
                "only",
            ],
        )
        .await,
    );
    let draft = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .into_iter()
        .find(|review| review.status == "draft")
        .unwrap();
    assert_eq!(draft.summary, "Overall only");

    ts.client
        .write_branch_artifact(
            &session.branch.id,
            "design",
            &ArtifactUpsertReq {
                content: "# Design\n\nAlpha beta gamma.\n\nCurrent revision.\n".to_string(),
                title: None,
                kind: None,
                author: Some("agent".to_string()),
                repo: false,
            },
        )
        .await
        .unwrap();
    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "retarget",
                &draft.id.to_string(),
                "--revision",
                &draft.draft_revision.to_string(),
            ],
        )
        .await,
    );
    let overall = ts.client.get_review(draft.id).await.unwrap();
    assert_eq!(overall.subject.version, "2");
    assert!(!overall.outdated);
    let shown = loom_review_cli(&ts, &["review", "show", &draft.id.to_string()]).await;
    assert_cli_ok(&shown);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["summary"], "Overall only");
    assert_eq!(shown["subject"]["version"], "2");
    assert!(shown["message"].as_str().unwrap().contains("revision 2"));
    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "submit",
                &overall.id.to_string(),
                "--revision",
                &overall.draft_revision.to_string(),
            ],
        )
        .await,
    );

    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "add",
                &session.id,
                "design",
                "--rev",
                "2",
                "--quote",
                "beta",
                "--prefix",
                "Alpha ",
                "--suffix",
                " gamma",
                "--block",
                "1",
                "Inline",
                "note",
            ],
        )
        .await,
    );
    let draft = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .into_iter()
        .find(|review| review.status == "draft")
        .unwrap();
    let comment_id = draft.comments[0].id.to_string();
    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "edit",
                &draft.id.to_string(),
                &comment_id,
                "--revision",
                &draft.draft_revision.to_string(),
                "Edited",
                "inline",
                "note",
            ],
        )
        .await,
    );
    let draft = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .into_iter()
        .find(|review| review.status == "draft")
        .unwrap();
    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "submit",
                &draft.id.to_string(),
                "--revision",
                &draft.draft_revision.to_string(),
            ],
        )
        .await,
    );

    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "overall",
                &session.id,
                "design",
                "--rev",
                "2",
                "Discard",
                "me",
            ],
        )
        .await,
    );
    let draft = ts
        .client
        .list_session_reviews(&session.id, "artifact", "design")
        .await
        .unwrap()
        .into_iter()
        .find(|review| review.status == "draft")
        .unwrap();
    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "discard",
                &draft.id.to_string(),
                "--revision",
                &draft.draft_revision.to_string(),
            ],
        )
        .await,
    );
}
