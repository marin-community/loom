//! Three process-boundary journeys for staged artifact reviews.
//!
//! Pure draft, identity, lease, and concurrency behavior belongs in
//! `weaver_core::review` and `loom::review_delivery` module tests. These tests
//! retain only the REST/CLI, ACP, and terminal wiring boundaries.

use reqwest::StatusCode;
use serde_json::{json, Value};
use serial_test::serial;
use std::os::unix::fs::MetadataExt;
use std::{process::Output, time::Duration};
use tokio::process::Command;
use weaver_api::{
    ArtifactTextAnchorDto, ChangeAnchorDto, ChangeSideDto, ChangeSourceDto, ReviewAnchorDto,
    ReviewAnchorKindDto, ReviewSubjectKindDto,
};

use super::fixtures::TestServer;
use weaver_api::operations::artifacts;
use weaver_api::operations::{reviews, sessions};

async fn insert_api_session(ts: &TestServer, id: &str) -> weaver_api::SessionView {
    let branch =
        weaver_core::branch::upsert(&ts.state.db, &ts.cwd(), &format!("weaver/{id}"), "main")
            .await
            .unwrap();
    loom::session::insert(
        &ts.state.db,
        &loom::session::NewSession {
            id: id.to_string(),
            branch_id: branch.id,
            work_dir: ts.cwd(),
            term_session: format!("weaver-{id}"),
            agent_kind: "shell".to_string(),
            model: String::new(),
            effort: String::new(),
            status: "orphaned".to_string(),
            github_repo: None,
            parent_branch_id: None,
            managed_by: None,
            created_by: Some("rjpower".to_string()),
            protocol: "terminal".to_string(),
            origin: "user".to_string(),
            class: "interactive".to_string(),
            tracking_issue_id: None,
        },
    )
    .await
    .unwrap();
    ts.client
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: id.to_string(),
        })
        .await
        .unwrap()
}

async fn seed_artifact(ts: &TestServer, session: &weaver_api::SessionView) {
    ts.client
        .invoke::<artifacts::write::Op>(&artifacts::write::Input {
            name: "design".to_string(),
            content: ("# Design\n\nAlpha beta gamma.\n".to_string()).clone(),
            title: (Some("Design".to_string())).clone(),
            kind: (Some("markdown".to_string())).clone(),
            base_rev: None,
            repo: false,
            branch: session.branch.id.to_string(),
        })
        .await
        .unwrap();
}

fn new_review(session: &str, version: &str) -> reviews::create::Input {
    reviews::create::Input {
        session: session.to_string(),
        subject_kind: ReviewSubjectKindDto::Artifact,
        subject_key: "design".to_string(),
        subject_version: version.to_string(),
    }
}

fn comment(
    id: i64,
    expected_revision: i64,
    version: &str,
    body: &str,
) -> reviews::comments::create::Input {
    reviews::comments::create::Input {
        id,
        expected_revision,
        subject_version: version.to_string(),
        anchor_kind: ReviewAnchorKindDto::Text,
        anchor: ReviewAnchorDto::Text(ArtifactTextAnchorDto {
            quote: "beta".to_string(),
            prefix: "Alpha ".to_string(),
            suffix: " gamma".to_string(),
            block_index: Some(1),
        }),
        body: body.to_string(),
    }
}

async fn draft_with_comment(
    ts: &TestServer,
    session: &weaver_api::SessionView,
    body: &str,
) -> weaver_api::ReviewDto {
    let draft = ts
        .client
        .invoke::<reviews::create::Op>(&new_review(&session.id, "1"))
        .await
        .unwrap();
    ts.client
        .invoke::<reviews::comments::create::Op>(&comment(
            draft.id,
            draft.draft_revision,
            "1",
            body,
        ))
        .await
        .unwrap()
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

async fn loom_review_cli(ts: &TestServer, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(args)
        .env("WEAVER_API", format!("http://{}", ts.addr))
        .output()
        .await
        .unwrap()
}

fn assert_cli_ok(output: &Output) {
    assert!(
        output.status.success(),
        "CLI failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_and_cli_share_the_private_optimistic_review_contract() {
    let ts = TestServer::start().await;
    super::fixtures::sh(ts.repo_path(), "git", &["switch", "-c", "weaver/reviewapi"]);
    std::fs::write(ts.repo_path().join("committed.txt"), "committed\n").unwrap();
    super::fixtures::sh(ts.repo_path(), "git", &["add", "committed.txt"]);
    super::fixtures::sh(ts.repo_path(), "git", &["commit", "-m", "branch change"]);
    std::fs::write(ts.repo_path().join("README.md"), "hello\nstaged\n").unwrap();
    super::fixtures::sh(ts.repo_path(), "git", &["add", "README.md"]);
    std::fs::write(
        ts.repo_path().join("README.md"),
        "hello\nstaged\nunstaged\n",
    )
    .unwrap();
    std::fs::write(ts.repo_path().join("untracked.txt"), "untracked\n").unwrap();
    let session = insert_api_session(&ts, "reviewapi").await;
    seed_artifact(&ts, &session).await;

    let draft = ts
        .client
        .invoke::<reviews::create::Op>(&new_review(&session.id, "01"))
        .await
        .unwrap();
    assert_eq!(draft.subject.version, "1");
    assert_eq!(draft.subject.key, "design");
    assert!(draft.subject.id.parse::<i64>().unwrap() > 0);
    let summarized = ts
        .client
        .invoke::<reviews::update::Op>(&reviews::update::Input {
            id: draft.id,
            expected_revision: draft.draft_revision,
            summary: (Some("Review the safety argument.".to_string())).clone(),
            subject_version: None.clone(),
        })
        .await
        .unwrap();
    let added = ts
        .client
        .invoke::<reviews::comments::create::Op>(&comment(
            draft.id,
            summarized.draft_revision,
            "01",
            "Explain why this is safe.",
        ))
        .await
        .unwrap();
    assert_eq!(added.comments[0].subject_version, "1");
    assert!(added.message.contains("\"prefix\":\"Alpha \""));

    let session_token = loom::auth::create_session_token(
        &ts.state.db,
        Some("rjpower"),
        &session.id,
        &session.branch.id,
    )
    .await
    .unwrap();
    let session_client =
        weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(session_token));
    assert!(
        session_client
            .invoke::<reviews::list::Op>(&reviews::list::Input {
                subject_kind: "artifact".parse().unwrap(),
                subject_key: "design".to_string(),
                session: session.id.to_string(),
            })
            .await
            .unwrap()
            .is_empty(),
        "a same-owner session credential must not inherit the operator's draft"
    );

    loom::auth::add_user(&ts.state.db, "bob", None, None, loom::auth::UserRole::Admin)
        .await
        .unwrap();
    let (token, _) = loom::auth::create_token(&ts.state.db, "bob", "reviewer", None)
        .await
        .unwrap();
    let bob =
        weaver_api::Client::new(format!("http://{}", ts.addr)).with_token(Some(token.clone()));
    assert!(bob
        .invoke::<reviews::list::Op>(&reviews::list::Input {
            subject_kind: "artifact".parse().unwrap(),
            subject_key: "design".to_string(),
            session: session.id.to_string(),
        })
        .await
        .unwrap()
        .is_empty());
    let hidden_mutation = reqwest::Client::new()
        .post(format!("http://{}/api/reviews/update", ts.addr))
        .bearer_auth(&token)
        .json(&json!({
            "id": draft.id,
            "expected_revision": added.draft_revision,
            "summary": "not mine",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(hidden_mutation.status(), StatusCode::NOT_FOUND);

    let stale_mutation = reqwest::Client::new()
        .post(format!("http://{}/api/reviews/comments/update", ts.addr))
        .json(&json!({
            "id": draft.id,
            "comment_id": added.comments[0].id,
            "expected_revision": summarized.draft_revision,
            "body": "unseen overwrite",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_mutation.status(), StatusCode::CONFLICT);
    let conflict: Value = stale_mutation.json().await.unwrap();
    assert_eq!(
        conflict["details"]["review"]["draft_revision"],
        added.draft_revision
    );

    ts.client
        .invoke::<artifacts::write::Op>(&artifacts::write::Input {
            name: "design".to_string(),
            content: ("# Design\n\nAlpha beta gamma, revised.\n".to_string()).clone(),
            title: None.clone(),
            kind: None.clone(),
            base_rev: None,
            repo: false,
            branch: session.branch.id.to_string(),
        })
        .await
        .unwrap();
    let outdated = ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: draft.id,
            expected_revision: added.draft_revision,
            acknowledge_outdated: false,
        })
        .await
        .unwrap_err();
    assert!(outdated.to_string().contains("outdated"));

    let reanchored = ts
        .client
        .invoke::<reviews::comments::update::Op>(&reviews::comments::update::Input {
            id: draft.id,
            comment_id: added.comments[0].id,
            expected_revision: added.draft_revision,
            body: None.clone(),
            subject_version: (Some("02".to_string())).clone(),
            anchor_kind: (Some(ReviewAnchorKindDto::Text)),
            anchor: (Some(ReviewAnchorDto::Text(ArtifactTextAnchorDto {
                quote: "beta gamma, revised".to_string(),
                prefix: "Alpha ".to_string(),
                suffix: ".".to_string(),
                block_index: Some(1),
            })))
            .clone(),
        })
        .await
        .unwrap();
    assert_eq!(reanchored.subject.version, "2");
    assert_eq!(reanchored.comments[0].subject_version, "2");
    assert!(!reanchored.outdated);
    let preview = reanchored.message.clone();
    let submitted = ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: draft.id,
            expected_revision: reanchored.draft_revision,
            acknowledge_outdated: false,
        })
        .await
        .unwrap();
    assert_eq!(submitted.message, preview);
    assert_eq!(submitted.delivery_state, "queued");
    assert!(
        session_client
            .invoke::<reviews::list::Op>(&reviews::list::Input {
                subject_kind: "artifact".parse().unwrap(),
                subject_key: "design".to_string(),
                session: session.id.to_string(),
            })
            .await
            .unwrap()
            .iter()
            .any(|review| review.id == submitted.id),
        "the submitted review is visible to its session credential"
    );

    let stale_retry = reqwest::Client::new()
        .post(format!("http://{}/api/reviews/submit", ts.addr))
        .json(&json!({
            "id": draft.id,
            "expected_revision": 0,
            "acknowledge_outdated": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_retry.status(), StatusCode::CONFLICT);
    assert_eq!(
        ts.client
            .invoke::<reviews::submit::Op>(&reviews::submit::Input {
                id: draft.id,
                expected_revision: reanchored.draft_revision,
                acknowledge_outdated: true,
            })
            .await
            .unwrap()
            .message,
        preview
    );
    let (events, deliveries): (i64, i64) = (
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM events
             WHERE branch_id = ? AND kind = 'review_submitted'",
        )
        .bind(&session.branch.id)
        .fetch_one(&ts.state.db)
        .await
        .unwrap(),
        sqlx::query_scalar("SELECT COUNT(*) FROM review_delivery_outbox WHERE review_id = ?")
            .bind(draft.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap(),
    );
    assert_eq!((events, deliveries), (1, 1));

    let visible = bob
        .invoke::<reviews::list::Op>(&reviews::list::Input {
            subject_kind: "artifact".parse().unwrap(),
            subject_key: "design".to_string(),
            session: session.id.to_string(),
        })
        .await
        .unwrap();
    assert!(visible.iter().any(|review| review.id == submitted.id));
    let comment_id = submitted.comments[0].id;
    assert_eq!(
        bob.invoke::<reviews::comments::resolve::Op>(&reviews::comments::resolve::Input {
            id: submitted.id,
            comment_id,
            resolved: true,
        })
        .await
        .unwrap()
        .status,
        "resolved"
    );
    assert_eq!(
        bob.invoke::<reviews::comments::resolve::Op>(&reviews::comments::resolve::Input {
            id: submitted.id,
            comment_id,
            resolved: false,
        })
        .await
        .unwrap()
        .status,
        "submitted"
    );

    let overall = loom_review_cli(
        &ts,
        &[
            "review",
            "overall",
            &session.id,
            "design",
            "--rev",
            "2",
            "CLI",
            "overall",
        ],
    )
    .await;
    assert_cli_ok(&overall);
    let cli_draft = ts
        .client
        .invoke::<reviews::list::Op>(&reviews::list::Input {
            subject_kind: "artifact".parse().unwrap(),
            subject_key: "design".to_string(),
            session: session.id.to_string(),
        })
        .await
        .unwrap()
        .into_iter()
        .find(|review| review.status == "draft")
        .unwrap();
    ts.client
        .invoke::<artifacts::write::Op>(&artifacts::write::Input {
            name: "design".to_string(),
            content: ("# Design\n\nThird revision.\n".to_string()).clone(),
            title: None.clone(),
            kind: None.clone(),
            base_rev: None,
            repo: false,
            branch: session.branch.id.to_string(),
        })
        .await
        .unwrap();
    assert_cli_ok(
        &loom_review_cli(
            &ts,
            &[
                "review",
                "retarget",
                &cli_draft.id.to_string(),
                "--revision",
                &cli_draft.draft_revision.to_string(),
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
                "3",
                "--quote",
                "Third",
                "--prefix",
                "# Design\n\n",
                "--suffix",
                " revision.",
                "--block",
                "1",
                "CLI",
                "anchored",
                "body",
            ],
        )
        .await,
    );
    let shown = loom_review_cli(&ts, &["review", "show", &cli_draft.id.to_string()]).await;
    assert_cli_ok(&shown);
    let shown: Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["subject"]["version"], "3");
    assert_eq!(shown["summary"], "CLI overall");
    assert_eq!(shown["comments"][0]["anchor"]["quote"], "Third");
    assert_eq!(shown["comments"][0]["anchor"]["prefix"], "# Design\n\n");
    assert_eq!(shown["comments"][0]["anchor"]["suffix"], " revision.");
    assert_eq!(shown["comments"][0]["body"], "CLI anchored body");

    let index_path = ts.repo_path().join(".git/index");
    let index_before = std::fs::read(&index_path).unwrap();
    let index_stat_before = std::fs::metadata(&index_path).unwrap();
    let changes = ts
        .client
        .invoke::<sessions::changes::Op>(&sessions::changes::Input {
            session: session.id.to_string(),
        })
        .await
        .unwrap();
    let version = changes.version.clone().unwrap();
    let readme = changes
        .files
        .iter()
        .find(|file| file.path.display == "README.md")
        .unwrap();
    assert_eq!(
        readme.sources,
        vec![ChangeSourceDto::Staged, ChangeSourceDto::Unstaged]
    );
    assert!(changes.files.iter().any(|file| {
        file.path.display == "committed.txt" && file.sources == vec![ChangeSourceDto::Committed]
    }));
    assert!(changes.files.iter().any(|file| {
        file.path.display == "untracked.txt" && file.sources == vec![ChangeSourceDto::Untracked]
    }));
    let cli_changes = loom_review_cli(&ts, &["session", "changes", &session.id]).await;
    assert_cli_ok(&cli_changes);
    let cli_changes: weaver_api::ChangeSetDto =
        serde_json::from_slice(&cli_changes.stdout).unwrap();
    assert_eq!(cli_changes.version.as_deref(), Some(version.as_str()));
    let index_stat_after = std::fs::metadata(&index_path).unwrap();
    assert_eq!(std::fs::read(&index_path).unwrap(), index_before);
    assert_eq!(
        (
            index_stat_after.dev(),
            index_stat_after.ino(),
            index_stat_after.mode(),
            index_stat_after.len(),
            index_stat_after.mtime(),
            index_stat_after.mtime_nsec(),
            index_stat_after.ctime(),
            index_stat_after.ctime_nsec(),
        ),
        (
            index_stat_before.dev(),
            index_stat_before.ino(),
            index_stat_before.mode(),
            index_stat_before.len(),
            index_stat_before.mtime(),
            index_stat_before.mtime_nsec(),
            index_stat_before.ctime(),
            index_stat_before.ctime_nsec(),
        ),
        "Changes reads must not refresh or rewrite the real index"
    );

    let line = readme
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .find(|line| line.new_line == Some(2))
        .unwrap();
    let change_draft = ts
        .client
        .invoke::<reviews::create::Op>(&reviews::create::Input {
            session: session.id.to_string(),
            subject_kind: ReviewSubjectKindDto::Changes,
            subject_key: ("changes".to_string()).clone(),
            subject_version: version.clone(),
        })
        .await
        .unwrap();
    let change_draft = ts
        .client
        .invoke::<reviews::comments::create::Op>(&reviews::comments::create::Input {
            id: change_draft.id,
            expected_revision: change_draft.draft_revision,
            subject_version: version.clone(),
            anchor_kind: ReviewAnchorKindDto::Change,
            anchor: (ReviewAnchorDto::Change(ChangeAnchorDto {
                path: readme.path.clone(),
                side: ChangeSideDto::New,
                start_line: 2,
                end_line: 2,
                hunk_header: readme.hunks[0].header.clone(),
                context_before: vec!["hello".to_string()],
                selected: vec![line.text.clone()],
                context_after: vec!["unstaged".to_string()],
            }))
            .clone(),
            body: ("Explain this staged line.".to_string()).clone(),
        })
        .await
        .unwrap();
    std::fs::write(
        ts.repo_path().join("README.md"),
        "hello\nstaged\nunstaged\nmoved\n",
    )
    .unwrap();
    assert!(ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: change_draft.id,
            expected_revision: change_draft.draft_revision,
            acknowledge_outdated: false,
        })
        .await
        .unwrap_err()
        .to_string()
        .contains("outdated"));
    let submitted = ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: change_draft.id,
            expected_revision: change_draft.draft_revision,
            acknowledge_outdated: true,
        })
        .await
        .unwrap();
    let ReviewAnchorDto::Change(anchor) = &submitted.comments[0].anchor else {
        panic!("submitted changes review lost its typed anchor");
    };
    assert_eq!(anchor.path, readme.path);
    assert_eq!(submitted.delivery_state, "queued");
}

async fn poll_review_turn(ts: &TestServer, session_id: &str, payload: &str) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let chat = ts
            .client
            .post("/api/sessions/chat", json!({ "session": session_id }))
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

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_delivery_has_one_protected_crash_boundary_and_can_rehome() {
    let ts = TestServer::start().await;
    super::acp::start_new(&ts, "acp-review", None, None).await;
    let session = ts
        .client
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: "acp-review".to_string(),
        })
        .await
        .unwrap();
    seed_artifact(&ts, &session).await;
    ts.client
        .post(
            "/api/sessions/prompt/create",
            json!({ "session": "acp-review", "text": "wait:1200|say:first" }),
        )
        .await
        .unwrap();

    let draft = draft_with_comment(&ts, &session, "Protected immutable feedback.").await;
    let payload = draft.message.clone();
    let submitted = ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: draft.id,
            expected_revision: draft.draft_revision,
            acknowledge_outdated: false,
        })
        .await
        .unwrap();
    assert_eq!(submitted.delivery_state, "delivered");
    let chat = poll_review_turn(&ts, &session.id, &payload).await;
    let protected = chat["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["kind"] == "user_message" && block["payload"]["text"] == payload)
        .unwrap();
    assert_eq!(protected["payload"]["delivery_key"], submitted.delivery_key);

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
    tokio::time::sleep(Duration::from_millis(100)).await;
    let chat = ts
        .client
        .post("/api/sessions/chat", json!({ "session": "acp-review" }))
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
    let inbox: (String, i64) = sqlx::query_as(
        "SELECT state, COUNT(*) FROM review_conversation_inbox WHERE delivery_key = ?",
    )
    .bind(&submitted.delivery_key)
    .fetch_one(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(inbox, ("consumed".to_string(), 1));

    super::acp::start_new(&ts, "acp-review-rehome", None, None).await;
    let original = ts
        .client
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: "acp-review-rehome".to_string(),
        })
        .await
        .unwrap();
    seed_artifact(&ts, &original).await;
    ts.client
        .post(
            "/api/sessions/prompt/create",
            json!({ "session": "acp-review-rehome", "text": "wait:30000|say:keep-busy" }),
        )
        .await
        .unwrap();
    // A standalone Stop deliberately pauses automatic delivery. This leaves
    // the subsequently submitted review in the protected inbox so a successor
    // can prove the cross-runtime rehome path.
    ts.client
        .post(
            "/api/sessions/interrupt",
            json!({ "session": "acp-review-rehome" }),
        )
        .await
        .unwrap();
    let rehomed = draft_with_comment(&ts, &original, "Rehome this immutable review.").await;
    let rehome_payload = rehomed.message.clone();
    let rehomed = ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: rehomed.id,
            expected_revision: rehomed.draft_revision,
            acknowledge_outdated: false,
        })
        .await
        .unwrap();
    assert!(ts.state.acp.stop(&original.id));
    sqlx::query("UPDATE sessions SET status = 'archived' WHERE id = ?")
        .bind(&original.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let terminal = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            goal: (Some("terminal review successor".to_string())).clone(),
            cwd: (ts.cwd()).clone(),
            agent: (Some("shell".to_string())).clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    sqlx::query("UPDATE sessions SET branch_id = ? WHERE id = ?")
        .bind(&original.branch.id)
        .bind(&terminal.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    loom::review_delivery::drain(&ts.state).await.unwrap();
    let consumed: (String, String, String) = sqlx::query_as(
        "SELECT state, claimed_session_id, payload
         FROM review_conversation_inbox WHERE delivery_key = ?",
    )
    .bind(&rehomed.delivery_key)
    .fetch_one(&ts.state.db)
    .await
    .unwrap();
    assert_eq!(
        consumed,
        ("consumed".to_string(), terminal.id, rehome_payload)
    );
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_delivery_queues_offline_recovers_and_reports_real_failure() {
    let ts = TestServer::start().await;
    let session = ts
        .client
        .invoke::<sessions::launch::Op>(&sessions::launch::Input {
            goal: (Some("review an artifact".to_string())).clone(),
            cwd: (ts.cwd()).clone(),
            agent: (Some("shell".to_string())).clone(),
            ..Default::default()
        })
        .await
        .unwrap();
    seed_artifact(&ts, &session).await;
    let original_terminal: String =
        sqlx::query_scalar("SELECT term_session FROM sessions WHERE id = ?")
            .bind(&session.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    let offline = draft_with_comment(&ts, &session, "Queue while offline.").await;
    sqlx::query(
        "UPDATE sessions
         SET term_session = 'missing-review-terminal', status = 'orphaned'
         WHERE id = ?",
    )
    .bind(&session.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    let offline = ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: offline.id,
            expected_revision: offline.draft_revision,
            acknowledge_outdated: false,
        })
        .await
        .unwrap();
    let attempts: i64 =
        sqlx::query_scalar("SELECT attempts FROM review_delivery_outbox WHERE review_id = ?")
            .bind(offline.id)
            .fetch_one(&ts.state.db)
            .await
            .unwrap();
    assert_eq!((offline.delivery_state.as_str(), attempts), ("queued", 0));

    sqlx::query("UPDATE sessions SET term_session = ?, status = 'running' WHERE id = ?")
        .bind(original_terminal)
        .bind(&session.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    make_delivery_due(&ts.state.db, offline.id).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let review = ts
            .client
            .invoke::<reviews::get::Op>(&reviews::get::Input { id: offline.id })
            .await
            .unwrap();
        let screen = ts.client.preview(&session.id, 1_000).await.unwrap();
        let marker_count = screen.matches(&offline.delivery_key).count();
        if review.delivery_state == "delivered" && marker_count == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "background sweep did not deliver one structured marker: \
             state={}, marker_count={marker_count}\n{screen}",
            review.delivery_state
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    super::acp::start_new(&ts, "review-transport-failure", None, None).await;
    let incompatible = ts
        .client
        .invoke::<sessions::get::Op>(&sessions::get::Input {
            session: "review-transport-failure".to_string(),
        })
        .await
        .unwrap();
    seed_artifact(&ts, &incompatible).await;
    sqlx::query("UPDATE sessions SET protocol = 'terminal' WHERE id = ?")
        .bind(&incompatible.id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let failing =
        draft_with_comment(&ts, &incompatible, "Exercise an actual rejected transport.").await;
    let failing = ts
        .client
        .invoke::<reviews::submit::Op>(&reviews::submit::Input {
            id: failing.id,
            expected_revision: failing.draft_revision,
            acknowledge_outdated: false,
        })
        .await
        .unwrap();
    assert_eq!(failing.delivery_state, "retrying");
    for expected in ["retrying", "failed"] {
        make_delivery_due(&ts.state.db, failing.id).await;
        let error = loom::review_delivery::deliver_review(&ts.state, failing.id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("pasting review feedback"));
        assert_eq!(
            ts.client
                .invoke::<reviews::get::Op>(&reviews::get::Input { id: failing.id })
                .await
                .unwrap()
                .delivery_state,
            expected
        );
    }
    assert_eq!(
        ts.client
            .invoke::<reviews::retry_delivery::Op>(&reviews::retry_delivery::Input {
                id: failing.id
            })
            .await
            .unwrap()
            .delivery_state,
        "failed"
    );
}
