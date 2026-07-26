use futures_util::StreamExt;
use std::process::Command;
use std::time::Duration;

use serde_json::json;
use serial_test::serial;
use weaver_api::{
    CreateSessionGroupReq, CreateSessionSpaceReq, DeleteSessionGroupReq, DeleteSessionSpaceReq,
    MoveSessionsReq, ReorderSessionLayoutReq, RestoreSessionGroupsReq, SessionGroupOrderReq,
    SessionLayoutItemKind, SessionPlacementSelectorKind, SetSessionPlacementDefaultReq,
    UpdateSessionGroupReq, UpdateSessionSpaceReq,
};

use crate::fixtures::TestServer;

fn layout_cli(ts: &TestServer, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["session", "layout"])
        .args(args)
        .env("WEAVER_API", ts.addr.to_string())
        .output()
        .expect("running loom session layout command")
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layout_crud_moves_conflicts_search_events_and_cli_share_one_contract() {
    let ts = TestServer::start().await;
    let first = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "find this durable prompt",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "layout-first"
            }),
        )
        .await
        .unwrap();
    let second = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "second placement",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "layout-second"
            }),
        )
        .await
        .unwrap();
    let first_id = first["id"].as_str().unwrap();
    let second_id = second["id"].as_str().unwrap();
    assert_eq!(first["placement"]["space_name"], "User");
    assert_eq!(first["placement"]["group_name"], "Inbox");

    let initial = ts.client.get("/api/session-layout").await.unwrap();
    assert_eq!(initial["spaces"].as_array().unwrap().len(), 3);
    let initial_revision = initial["revision"].as_i64().unwrap();

    let with_group = ts
        .client
        .post(
            "/api/session-layout/groups",
            json!({
                "space_id": "space-user",
                "name": "Focused",
                "expected_revision": initial_revision
            }),
        )
        .await
        .unwrap();
    let focused = with_group["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|space| space["groups"].as_array().unwrap())
        .find(|group| group["name"] == "Focused")
        .unwrap();
    let focused_id = focused["id"].as_str().unwrap().to_string();
    let revision = with_group["revision"].as_i64().unwrap();

    let stale = reqwest::Client::new()
        .post(format!("http://{}/api/session-layout/moves", ts.addr))
        .json(&json!({
            "session_ids": [first_id],
            "destination_group_id": focused_id,
            "expected_revision": initial_revision
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);
    let stale_body: serde_json::Value = stale.json().await.unwrap();
    assert_eq!(stale_body["layout"]["revision"], revision);

    let mut events = ts.state.bus.subscribe();
    let moved = ts
        .client
        .post(
            "/api/session-layout/moves",
            json!({
                "session_ids": [first_id],
                "destination_group_id": focused_id,
                "expected_revision": revision
            }),
        )
        .await
        .unwrap();
    let move_revision = moved["revision"].as_i64().unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.kind == "session_layout" {
                break event;
            }
        }
    })
    .await
    .expect("layout mutation should publish a fleet event");
    assert_eq!(event.data["revision"], move_revision);

    let refreshed = ts
        .client
        .get(&format!("/api/sessions/{first_id}"))
        .await
        .unwrap();
    assert_eq!(refreshed["placement"]["group_name"], "Focused");
    assert_eq!(refreshed["origin"], first["origin"]);
    assert_eq!(refreshed["class"], first["class"]);
    assert_eq!(refreshed["parent_id"], first["parent_id"]);
    assert_eq!(refreshed["branch"]["branch"], first["branch"]["branch"]);

    let search = ts
        .client
        .get("/api/sessions/search?q=focused")
        .await
        .unwrap();
    assert!(
        search
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["id"] == first_id),
        "search includes qualified placement names"
    );
    let prompt_search = ts
        .client
        .get("/api/sessions/search?q=durable%20prompt")
        .await
        .unwrap();
    assert_eq!(prompt_search.as_array().unwrap().len(), 1);
    let field_name_search = ts
        .client
        .get("/api/sessions/search?q=mcp_policy")
        .await
        .unwrap();
    assert!(
        field_name_search.as_array().unwrap().is_empty(),
        "JSON field names are not searchable values"
    );
    ts.client
        .put(
            &format!("/api/sessions/{first_id}/tags/triage"),
            json!({ "value": "blocked", "note": "needs a decision", "by": "test" }),
        )
        .await
        .unwrap();
    let blocked = ts
        .client
        .get("/api/sessions/search?q=&attention=blocked")
        .await
        .unwrap();
    assert_eq!(blocked.as_array().unwrap().len(), 1);
    let needs = ts
        .client
        .get("/api/sessions/search?q=&attention=needs")
        .await
        .unwrap();
    assert_eq!(needs.as_array().unwrap().len(), 1);
    let tag_note = ts
        .client
        .get("/api/sessions/search?q=needs%20a%20decision")
        .await
        .unwrap();
    assert_eq!(
        tag_note.as_array().unwrap().len(),
        1,
        "nested tag values remain searchable"
    );

    let delete_error = reqwest::Client::new()
        .delete(format!(
            "http://{}/api/session-layout/groups/{focused_id}",
            ts.addr
        ))
        .json(&json!({
            "destination_group_id": null,
            "expected_revision": move_revision
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(delete_error.status(), reqwest::StatusCode::BAD_REQUEST);

    let cli = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args([
            "session",
            "layout",
            "move",
            "--to",
            "group-github-inbox",
            second_id,
        ])
        .env("WEAVER_API", ts.addr.to_string())
        .output()
        .expect("running loom session layout move");
    assert!(
        cli.status.success(),
        "layout CLI failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert!(String::from_utf8_lossy(&cli.stdout).contains("GitHub"));
    let second_after = ts
        .client
        .get(&format!("/api/sessions/{second_id}"))
        .await
        .unwrap();
    assert_eq!(second_after["placement"]["group_id"], "group-github-inbox");

    let current = ts.client.get_session_layout().await.unwrap();
    let relocated = ts
        .client
        .delete_session_group(
            &focused_id,
            &DeleteSessionGroupReq {
                destination_group_id: Some("group-user-inbox".to_string()),
                expected_revision: current.revision,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        ts.client
            .get(&format!("/api/sessions/{first_id}"))
            .await
            .unwrap()["placement"]["group_id"],
        "group-user-inbox"
    );

    let custom = ts
        .client
        .create_session_space(&CreateSessionSpaceReq {
            name: "Custom".to_string(),
            expected_revision: relocated.revision,
        })
        .await
        .unwrap();
    let custom_id = custom
        .spaces
        .iter()
        .find(|space| space.name == "Custom")
        .unwrap()
        .id
        .clone();
    let renamed_space = ts
        .client
        .update_session_space(
            &custom_id,
            &UpdateSessionSpaceReq {
                name: "Projects".to_string(),
                expected_revision: custom.revision,
            },
        )
        .await
        .unwrap();
    let added_group = ts
        .client
        .create_session_group(&CreateSessionGroupReq {
            space_id: custom_id.clone(),
            name: "Review".to_string(),
            expected_revision: renamed_space.revision,
        })
        .await
        .unwrap();
    let review_id = added_group
        .spaces
        .iter()
        .flat_map(|space| &space.groups)
        .find(|group| group.name == "Review")
        .unwrap()
        .id
        .clone();
    let with_default = ts
        .client
        .set_session_placement_default(&SetSessionPlacementDefaultReq {
            selector_kind: SessionPlacementSelectorKind::Origin,
            selector_value: "slack".to_string(),
            group_id: review_id.clone(),
            expected_revision: added_group.revision,
        })
        .await
        .unwrap();
    let without_default = ts
        .client
        .delete_session_placement_default(
            SessionPlacementSelectorKind::Origin,
            "slack",
            with_default.revision,
        )
        .await
        .unwrap();
    let custom_inbox = added_group
        .spaces
        .iter()
        .find(|space| space.id == custom_id)
        .unwrap()
        .groups
        .iter()
        .find(|group| group.name == "Inbox")
        .unwrap()
        .id
        .clone();
    let reordered = ts
        .client
        .reorder_session_layout(&ReorderSessionLayoutReq {
            kind: SessionLayoutItemKind::Group,
            id: review_id.clone(),
            before_id: Some(custom_inbox),
            destination_space_id: Some(custom_id.clone()),
            expected_revision: without_default.revision,
        })
        .await
        .unwrap();
    assert_eq!(
        reordered
            .spaces
            .iter()
            .find(|space| space.id == custom_id)
            .unwrap()
            .groups[0]
            .id,
        review_id
    );
    let renamed_group = ts
        .client
        .update_session_group(
            &review_id,
            &UpdateSessionGroupReq {
                name: "Ready".to_string(),
                expected_revision: reordered.revision,
            },
        )
        .await
        .unwrap();
    let without_group = ts
        .client
        .delete_session_group(
            &review_id,
            &DeleteSessionGroupReq {
                destination_group_id: None,
                expected_revision: renamed_group.revision,
            },
        )
        .await
        .unwrap();
    let without_space = ts
        .client
        .delete_session_space(
            &custom_id,
            &DeleteSessionSpaceReq {
                destination_group_id: None,
                expected_revision: without_group.revision,
            },
        )
        .await
        .unwrap();
    assert!(without_space
        .spaces
        .iter()
        .all(|space| space.id != custom_id));

    ts.client
        .post(&format!("/api/sessions/{first_id}/archive"), json!({}))
        .await
        .unwrap();
    let archived_needs = ts
        .client
        .get("/api/sessions/search?q=&history=true&attention=needs")
        .await
        .unwrap();
    assert!(
        archived_needs.as_array().unwrap().is_empty(),
        "archived sessions are calm even when they retain old attention tags"
    );
    let archived_only = ts
        .client
        .get("/api/sessions/search?q=&archived_only=true")
        .await
        .unwrap();
    assert_eq!(archived_only.as_array().unwrap().len(), 1);
    assert_eq!(archived_only[0]["id"], first_id);

    for id in [first_id, second_id] {
        ts.client
            .delete(&format!("/api/sessions/{id}"))
            .await
            .unwrap();
    }
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exact_parent_session_identity_survives_archive_and_parent_removal() {
    let ts = TestServer::start().await;
    let parent_one = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "first parent generation",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "lineage-parent"
            }),
        )
        .await
        .unwrap();
    let child_one = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "child of archived generation",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "lineage-child-one",
                "parent_branch": parent_one["branch"]["id"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(child_one["parent_session_id"], parent_one["id"]);
    ts.client
        .post(
            &format!(
                "/api/sessions/{}/archive",
                parent_one["id"].as_str().unwrap()
            ),
            json!({}),
        )
        .await
        .unwrap();

    let parent_two = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "second parent generation",
                "cwd": ts.cwd(),
                "agent": "shell",
                "existing_branch": parent_one["branch"]["branch"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(parent_two["branch"]["id"], parent_one["branch"]["id"]);
    let child_two = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "child of exact second generation",
                "cwd": ts.cwd(),
                "agent": "shell",
                "name": "lineage-child-two",
                "parent_branch": parent_two["branch"]["id"]
            }),
        )
        .await
        .unwrap();
    assert_eq!(child_two["parent_id"], parent_one["branch"]["id"]);
    assert_eq!(child_two["parent_session_id"], parent_two["id"]);
    let cli = Command::new(env!("CARGO_BIN_EXE_loom"))
        .args(["session", "show", child_two["id"].as_str().unwrap()])
        .env("WEAVER_API", ts.addr.to_string())
        .output()
        .expect("showing exact parent session through the CLI");
    assert!(cli.status.success());
    assert!(String::from_utf8_lossy(&cli.stdout).contains(&format!(
        "parent:   session {}",
        parent_two["id"].as_str().unwrap()
    )));
    let child_one_after_archive = ts
        .client
        .get(&format!(
            "/api/sessions/{}",
            child_one["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
    assert_eq!(
        child_one_after_archive["parent_session_id"], parent_one["id"],
        "archived exact parents remain addressable in History"
    );

    ts.client
        .delete(&format!(
            "/api/sessions/{}",
            parent_two["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
    let after_parent_delete = ts
        .client
        .get(&format!(
            "/api/sessions/{}",
            child_two["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
    assert_eq!(
        after_parent_delete["parent_session_id"], parent_two["id"],
        "exact provenance is immutable even after the parent row is removed"
    );

    sqlx::query("UPDATE sessions SET parent_session_id = NULL WHERE id = ?")
        .bind(child_two["id"].as_str().unwrap())
        .execute(&ts.state.db)
        .await
        .unwrap();
    let legacy = ts
        .client
        .get(&format!(
            "/api/sessions/{}",
            child_two["id"].as_str().unwrap()
        ))
        .await
        .unwrap();
    assert!(legacy["parent_session_id"].is_null());
    assert_eq!(legacy["parent_id"], parent_one["branch"]["id"]);
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canonical_defaults_legacy_reads_and_cross_space_collisions_are_consistent() {
    let ts = TestServer::start().await;
    let initial = ts.client.get_session_layout().await.unwrap();
    let with_default = ts
        .client
        .set_session_placement_default(&SetSessionPlacementDefaultReq {
            selector_kind: SessionPlacementSelectorKind::Profile,
            selector_value: "default".to_string(),
            group_id: "group-github-inbox".to_string(),
            expected_revision: initial.revision,
        })
        .await
        .unwrap();
    let created = ts
        .client
        .post(
            "/api/sessions",
            json!({
                "goal": "profile placement wins while another space is visible",
                "cwd": ts.cwd(),
                "agent": "shell"
            }),
        )
        .await
        .unwrap();
    let session_id = created["id"].as_str().unwrap();
    assert_eq!(created["placement"]["group_id"], "group-github-inbox");

    let later = ts
        .client
        .create_session_group(&CreateSessionGroupReq {
            space_id: "space-github".to_string(),
            name: "Later".to_string(),
            expected_revision: with_default.revision + 1,
        })
        .await
        .unwrap();
    let later_id = later
        .spaces
        .iter()
        .flat_map(|space| &space.groups)
        .find(|group| group.name == "Later")
        .unwrap()
        .id
        .clone();
    sqlx::query("UPDATE session_groups SET system_key = 'later' WHERE id = ?")
        .bind(&later_id)
        .execute(&ts.state.db)
        .await
        .unwrap();
    let moved = ts
        .client
        .move_sessions(&MoveSessionsReq {
            session_ids: vec![session_id.to_string()],
            destination_group_id: later_id.clone(),
            before_session_id: None,
            expected_revision: later.revision,
        })
        .await
        .unwrap();
    let legacy_read = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert_eq!(legacy_read["park"], "parked");
    assert_eq!(legacy_read["sort_order"], 0.0);

    let rejected = reqwest::Client::new()
        .patch(format!("http://{}/api/sessions/{session_id}", ts.addr))
        .json(&json!({
            "title": "must not partially apply",
            "park": "active",
            "sort_order": 42
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    let after_rejected = ts
        .client
        .get(&format!("/api/sessions/{session_id}"))
        .await
        .unwrap();
    assert_eq!(after_rejected["placement"]["group_id"], later_id);
    assert_ne!(
        after_rejected["branch"]["title"],
        "must not partially apply"
    );
    ts.client
        .patch(
            &format!("/api/sessions/{session_id}"),
            json!({ "status": "error" }),
        )
        .await
        .unwrap();
    let urgent = ts
        .client
        .get("/api/sessions/search?q=&attention=needs")
        .await
        .unwrap();
    assert!(urgent
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["id"] == session_id));
    ts.client
        .patch(
            &format!("/api/sessions/{session_id}"),
            json!({ "status": "running" }),
        )
        .await
        .unwrap();

    let user_review = ts
        .client
        .create_session_group(&CreateSessionGroupReq {
            space_id: "space-user".to_string(),
            name: "Review".to_string(),
            expected_revision: moved.revision,
        })
        .await
        .unwrap();
    let user_review_id = user_review
        .spaces
        .iter()
        .flat_map(|space| &space.groups)
        .find(|group| group.space_id == "space-user" && group.name == "Review")
        .unwrap()
        .id
        .clone();
    let github_review = ts
        .client
        .create_session_group(&CreateSessionGroupReq {
            space_id: "space-github".to_string(),
            name: "Review".to_string(),
            expected_revision: user_review.revision,
        })
        .await
        .unwrap();

    let name_collision = reqwest::Client::new()
        .post(format!("http://{}/api/session-layout/reorder", ts.addr))
        .json(&json!({
            "kind": "group",
            "id": user_review_id,
            "destination_space_id": "space-github",
            "expected_revision": github_review.revision
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(name_collision.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(name_collision
        .text()
        .await
        .unwrap()
        .contains("already has a group named 'Review'"));

    let system_collision = reqwest::Client::new()
        .post(format!("http://{}/api/session-layout/reorder", ts.addr))
        .json(&json!({
            "kind": "group",
            "id": "group-user-inbox",
            "destination_space_id": "space-github",
            "expected_revision": github_review.revision
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(system_collision.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(system_collision
        .text()
        .await
        .unwrap()
        .contains("system key 'inbox'"));

    let renamed = ts
        .client
        .update_session_group(
            &user_review_id,
            &UpdateSessionGroupReq {
                name: "Unique review".to_string(),
                expected_revision: github_review.revision,
            },
        )
        .await
        .unwrap();
    let crossed = ts
        .client
        .reorder_session_layout(&ReorderSessionLayoutReq {
            kind: SessionLayoutItemKind::Group,
            id: user_review_id.clone(),
            before_id: None,
            destination_space_id: Some("space-github".to_string()),
            expected_revision: renamed.revision,
        })
        .await
        .unwrap();
    assert!(crossed
        .spaces
        .iter()
        .find(|space| space.id == "space-github")
        .unwrap()
        .groups
        .iter()
        .any(|group| group.id == user_review_id));
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn atomic_group_restore_rolls_back_an_injected_mid_write_failure() {
    let ts = TestServer::start().await;
    let mut ids = Vec::new();
    for name in ["restore-a", "restore-b"] {
        let created = ts
            .client
            .post(
                "/api/sessions",
                json!({ "goal": name, "cwd": ts.cwd(), "agent": "shell", "name": name }),
            )
            .await
            .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    let layout = ts.client.get_session_layout().await.unwrap();
    let moved = ts
        .client
        .move_sessions(&MoveSessionsReq {
            session_ids: vec![ids[0].clone()],
            destination_group_id: "group-user-inbox".to_string(),
            before_session_id: None,
            expected_revision: layout.revision,
        })
        .await
        .unwrap();
    let current_order = moved
        .spaces
        .iter()
        .find(|space| space.id == "space-user")
        .unwrap()
        .groups
        .iter()
        .find(|group| group.id == "group-user-inbox")
        .unwrap()
        .session_ids
        .clone();
    assert_eq!(current_order, vec![ids[1].clone(), ids[0].clone()]);

    sqlx::query(&format!(
        "CREATE TRIGGER fail_restore_b
         BEFORE UPDATE ON session_placements
         WHEN NEW.session_id = '{}'
         BEGIN SELECT RAISE(FAIL, 'injected restore failure'); END",
        ids[1].replace('\'', "''")
    ))
    .execute(&ts.state.db)
    .await
    .unwrap();
    let failed = reqwest::Client::new()
        .post(format!("http://{}/api/session-layout/restores", ts.addr))
        .json(&RestoreSessionGroupsReq {
            groups: vec![SessionGroupOrderReq {
                group_id: "group-user-inbox".to_string(),
                session_ids: ids.clone(),
            }],
            expected_revision: moved.revision,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), reqwest::StatusCode::INTERNAL_SERVER_ERROR);
    let after = ts.client.get_session_layout().await.unwrap();
    let after_order = &after
        .spaces
        .iter()
        .find(|space| space.id == "space-user")
        .unwrap()
        .groups
        .iter()
        .find(|group| group.id == "group-user-inbox")
        .unwrap()
        .session_ids;
    assert_eq!(after_order, &current_order, "no partial restore committed");
    assert_eq!(after.revision, moved.revision);
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hidden_managed_placements_cannot_block_an_apparently_empty_group() {
    let ts = TestServer::start().await;
    let initial = ts.client.get_session_layout().await.unwrap();
    let with_group = ts
        .client
        .create_session_group(&CreateSessionGroupReq {
            space_id: "space-user".to_string(),
            name: "Warm repair".to_string(),
            expected_revision: initial.revision,
        })
        .await
        .unwrap();
    let group_id = with_group
        .spaces
        .iter()
        .flat_map(|space| &space.groups)
        .find(|group| group.name == "Warm repair")
        .unwrap()
        .id
        .clone();
    let repo_root = ts.cwd();
    let branch =
        weaver_core::branch::upsert(&ts.state.db, &repo_root, "weaver/hidden-warm", "main")
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO sessions
         (id, branch_id, work_dir, term_session, status, managed_by, origin, class)
         VALUES ('hidden-warm', ?, '/tmp/hidden-warm', 'hidden-warm', 'running',
                 'watch-1', 'watch', 'automation')",
    )
    .bind(branch.id)
    .execute(&ts.state.db)
    .await
    .unwrap();
    // Simulate the pre-correction split model. Organizer deletion must repair
    // this hidden membership instead of asking for an impossible visible move.
    sqlx::query(
        "INSERT INTO session_placements (session_id, group_id, rank, updated_at)
         VALUES ('hidden-warm', ?, 0, '2026-01-01T00:00:00.000Z')",
    )
    .bind(&group_id)
    .execute(&ts.state.db)
    .await
    .unwrap();

    let deleted = ts
        .client
        .delete_session_group(
            &group_id,
            &DeleteSessionGroupReq {
                destination_group_id: None,
                expected_revision: with_group.revision,
            },
        )
        .await
        .unwrap();
    assert!(deleted
        .spaces
        .iter()
        .flat_map(|space| &space.groups)
        .all(|group| group.id != group_id));
    assert!(loom::session_layout::placement(&ts.state.db, "hidden-warm")
        .await
        .unwrap()
        .is_none());
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_http_sse_invalidates_on_session_membership_changes() {
    let ts = TestServer::start().await;
    let (token, _) = loom::auth::create_token(&ts.state.db, "rjpower", "sse-test", None)
        .await
        .unwrap();
    ts.client
        .patch("/api/settings", json!({ "auth.trust_loopback": false }))
        .await
        .unwrap();
    let http = reqwest::Client::new();
    let response = http
        .get(format!("http://{}/api/session-layout/events", ts.addr))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let mut stream = response.bytes_stream();

    let created: serde_json::Value = http
        .post(format!("http://{}/api/sessions", ts.addr))
        .bearer_auth(&token)
        .json(&json!({ "goal": "SSE membership", "cwd": ts.cwd(), "agent": "shell" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let created_event = tokio::time::timeout(Duration::from_secs(3), async {
        let mut body = String::new();
        while !body.contains("session_layout") {
            body.push_str(&String::from_utf8_lossy(
                &stream.next().await.unwrap().unwrap(),
            ));
        }
        body
    })
    .await
    .expect("session insertion should invalidate the HTTP SSE stream");
    assert!(created_event.contains("\"revision\""));

    let deleted = http
        .delete(format!(
            "http://{}/api/sessions/{}",
            ts.addr,
            created["id"].as_str().unwrap()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut body = String::new();
        while !body.contains("session_layout") {
            body.push_str(&String::from_utf8_lossy(
                &stream.next().await.unwrap().unwrap(),
            ));
        }
    })
    .await
    .expect("session removal should invalidate the HTTP SSE stream");
}

#[serial]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layout_cli_covers_read_crud_reorder_preferences_defaults_restore_and_stale_conflicts() {
    let ts = TestServer::start().await;
    let show = layout_cli(&ts, &["show"]);
    assert!(show.status.success());
    assert!(String::from_utf8_lossy(&show.stdout).contains("revision"));

    assert!(layout_cli(&ts, &["space-add", "CLI space"])
        .status
        .success());
    let layout = ts.client.get_session_layout().await.unwrap();
    let space = layout
        .spaces
        .iter()
        .find(|space| space.name == "CLI space")
        .unwrap();
    let space_id = space.id.clone();
    let inbox_id = space.groups[0].id.clone();
    assert!(
        layout_cli(&ts, &["space-rename", &space_id, "CLI projects"])
            .status
            .success()
    );
    assert!(layout_cli(&ts, &["group-add", &space_id, "CLI review"])
        .status
        .success());
    let layout = ts.client.get_session_layout().await.unwrap();
    let review_id = layout
        .spaces
        .iter()
        .flat_map(|space| &space.groups)
        .find(|group| group.name == "CLI review")
        .unwrap()
        .id
        .clone();
    assert!(layout_cli(&ts, &["group-rename", &review_id, "CLI ready"])
        .status
        .success());
    assert!(layout_cli(
        &ts,
        &["reorder", "group", &review_id, "--before", &inbox_id]
    )
    .status
    .success());
    assert!(layout_cli(&ts, &["collapse", &review_id]).status.success());
    assert!(
        ts.client
            .get_session_layout()
            .await
            .unwrap()
            .spaces
            .iter()
            .flat_map(|space| &space.groups)
            .find(|group| group.id == review_id)
            .unwrap()
            .collapsed
    );
    assert!(layout_cli(&ts, &["expand", &review_id]).status.success());
    assert!(layout_cli(
        &ts,
        &["default-set", "origin", "cli-source", "--to", &review_id,],
    )
    .status
    .success());
    assert!(
        !layout_cli(
            &ts,
            &["default-set", "watch", "inert-watch", "--to", &review_id]
        )
        .status
        .success(),
        "the CLI must not advertise an inert warm-session selector"
    );
    assert!(layout_cli(&ts, &["default-delete", "origin", "cli-source"])
        .status
        .success());
    let snapshot = format!(r#"[{{"group_id":"{review_id}","session_ids":[]}}]"#);
    assert!(layout_cli(&ts, &["restore", &snapshot]).status.success());

    let stale = layout_cli(
        &ts,
        &["group-rename", &review_id, "stale", "--revision", "0"],
    );
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("revision changed"));

    assert!(layout_cli(&ts, &["group-delete", &review_id])
        .status
        .success());
    assert!(layout_cli(&ts, &["space-delete", &space_id])
        .status
        .success());
}
