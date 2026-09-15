use orchard_workspace_host::{HostError, WorkspaceHost};
use rmcp::{
    model::CallToolRequestParams,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::json;
use serde_json::{Map, Value};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;

fn packaged_br() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../resources/bin/br")
        .canonicalize()
        .expect("approved packaged br fixture")
}

fn create_workspace(host: &WorkspaceHost, name: &str) -> (String, String, PathBuf) {
    let result = host
        .call(
            "workspace_create",
            json!({"name":name,"owner_name":"Local owner"}),
        )
        .expect("create workspace");
    let workspace = &result["workspace"];
    let workspace_id = workspace["id"].as_str().unwrap().to_owned();
    let store_id = workspace["task_stores"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let root = PathBuf::from(workspace["root"].as_str().unwrap());
    (workspace_id, store_id, root)
}

fn mcp_arguments(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn mcp_call(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_owned()).with_arguments(mcp_arguments(arguments))
}

#[test]
fn mail_only_mode_is_honest_when_br_is_unavailable() {
    let temp = TempDir::new().unwrap();
    let host =
        WorkspaceHost::open(temp.path().join("data"), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Mail only"}))
        .unwrap();
    assert_eq!(created["workspace"]["repositories"], json!([]));
    assert_eq!(created["workspace"]["task_stores"], json!([]));
    assert_eq!(created["task_backend"]["available"], false);
    let workspace_id = created["workspace"]["id"].as_str().unwrap();
    let participants = host
        .call("mail_participants", json!({"workspace_id":workspace_id}))
        .unwrap();
    assert_eq!(participants["participants"].as_array().unwrap().len(), 2);
}

#[test]
fn snapshot_returns_newest_history_after_more_than_two_hundred_messages() {
    let temp = TempDir::new().unwrap();
    let host =
        WorkspaceHost::open(temp.path().join("data"), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Busy"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap();
    for index in 0..205 {
        host.call(
            "mail_send",
            json!({
                "workspace_id":workspace_id,
                "request_id":format!("busy-{index}"),
                "sender_id":"owner",
                "destination":{"kind":"channel","id":"general"},
                "body":format!("message-{index}")
            }),
        )
        .unwrap();
    }
    let snapshot = host
        .call(
            "workspace_snapshot",
            json!({"workspace_id":workspace_id,"history_limit":5}),
        )
        .unwrap();
    let history = snapshot["mail"]["history"].as_array().unwrap();
    assert_eq!(history.len(), 5);
    assert_eq!(history.first().unwrap()["body"], "message-200");
    assert_eq!(history.last().unwrap()["body"], "message-204");
}

#[test]
fn real_br_task_lifecycle_retries_external_refresh_and_duplicate_ids() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = WorkspaceHost::open(data_root.clone(), packaged_br()).unwrap();
    let (workspace_id, store_id, workspace_root) = create_workspace(&host, "Tasks");

    let created = host
        .call(
            "task_create",
            json!({
                "workspace_id":workspace_id,"store_id":store_id,"request_id":"create-1",
                "title":"Alpha","description":"First","priority":1,"labels":["one","two"]
            }),
        )
        .unwrap();
    let task_id = created["task"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["task"]["task_ref"]["store_id"], store_id);
    assert_eq!(created["application_status"], "br_success");

    let replay = host
        .call(
            "task_create",
            json!({
                "workspace_id":workspace_id,"store_id":store_id,"request_id":"create-1",
                "title":"Alpha","description":"First","priority":1,"labels":["one","two"]
            }),
        )
        .unwrap();
    assert_eq!(replay["task"]["id"], task_id);
    assert_eq!(replay["idempotent_replay"], true);
    let conflict = host.call(
        "task_create",
        json!({
            "workspace_id":workspace_id,"store_id":store_id,"request_id":"create-1",
            "title":"Different"
        }),
    );
    assert!(conflict.unwrap_err().contains("different task arguments"));

    let updated = host
        .call(
            "task_update",
            json!({
                "workspace_id":workspace_id,"store_id":store_id,"task_id":task_id,
                "request_id":"update-1","title":"Alpha two","add_labels":["three"]
            }),
        )
        .unwrap();
    assert_eq!(updated["task"]["description"], "First");
    assert_eq!(updated["task"]["title"], "Alpha two");

    let db_path = workspace_root.join("tasks/.beads/beads.db");
    let external = Command::new(packaged_br())
        .current_dir(workspace_root.join("tasks"))
        .args(["--db"])
        .arg(&db_path)
        .args(["--json", "update"])
        .arg(&task_id)
        .args(["--title", "External title"])
        .output()
        .unwrap();
    assert!(
        external.status.success(),
        "{}",
        String::from_utf8_lossy(&external.stderr)
    );
    let observed = host
        .call(
            "task_show",
            json!({"workspace_id":workspace_id,"store_id":store_id,"task_id":task_id}),
        )
        .unwrap();
    assert_eq!(observed["title"], "External title");

    let deps = host
        .call(
            "task_dependencies",
            json!({"workspace_id":workspace_id,"store_id":store_id,"task_id":task_id}),
        )
        .unwrap();
    assert_eq!(deps["task_ref"]["task_id"], task_id);

    let copied_store = temp.path().join("copied-store");
    copy_tree(&workspace_root.join("tasks"), &copied_store);
    let attached = host
        .call(
            "task_store_attach",
            json!({"workspace_id":workspace_id,"path":copied_store}),
        )
        .unwrap();
    let second_store = attached["task_store"]["id"].as_str().unwrap();
    let duplicate = host
        .call(
            "task_show",
            json!({"workspace_id":workspace_id,"store_id":second_store,"task_id":task_id}),
        )
        .unwrap();
    assert_eq!(duplicate["id"], task_id);
    assert_ne!(duplicate["task_ref"]["store_id"], store_id);

    let closed = host
        .call(
            "task_close",
            json!({
                "workspace_id":workspace_id,"store_id":store_id,"task_id":task_id,
                "request_id":"close-1","reason":"done"
            }),
        )
        .unwrap();
    assert_eq!(closed["task"]["status"], "closed");
    let jsonl = fs::read_to_string(workspace_root.join("tasks/.beads/issues.jsonl")).unwrap();
    assert!(jsonl.contains(&task_id));

    drop(host);
    let reopened = WorkspaceHost::open(data_root, packaged_br()).unwrap();
    let list = reopened.call("workspace_list", json!({})).unwrap();
    assert_eq!(list["workspaces"].as_array().unwrap().len(), 1);
    let persisted = reopened
        .call(
            "task_show",
            json!({"workspace_id":workspace_id,"store_id":store_id,"task_id":task_id}),
        )
        .unwrap();
    assert_eq!(persisted["status"], "closed");
}

#[test]
fn unsupported_schema_and_cross_workspace_alias_are_rejected_without_mutation() {
    let temp = TempDir::new().unwrap();
    let host = WorkspaceHost::open(temp.path().join("data"), packaged_br()).unwrap();
    let (first, _, first_root) = create_workspace(&host, "First");
    let (second, _, _) = create_workspace(&host, "Second");

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(first_root.join("tasks"), temp.path().join("store-alias"))
            .unwrap();
        let alias = host.call(
            "task_store_attach",
            json!({"workspace_id":second,"path":temp.path().join("store-alias")}),
        );
        assert!(alias.unwrap_err().contains("already attached to workspace"));
    }

    let unsupported = temp.path().join("unsupported/.beads");
    fs::create_dir_all(&unsupported).unwrap();
    let db_path = unsupported.join("beads.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "PRAGMA user_version=1; CREATE TABLE issues(id TEXT PRIMARY KEY, title TEXT); CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES ('unchanged');",
    )
    .unwrap();
    drop(db);
    let result = host.call(
        "task_store_attach",
        json!({"workspace_id":first,"path":unsupported.parent().unwrap()}),
    );
    assert!(result.unwrap_err().contains("unsupported Beads schema"));
    let db = rusqlite::Connection::open(&db_path).unwrap();
    let sentinel: String = db
        .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sentinel, "unchanged");
    let columns: i64 = db
        .query_row(
            "SELECT count(*) FROM pragma_table_info('issues')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_tokens_isolate_rotate_archive_and_survive_restart() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = Arc::new(WorkspaceHost::open(data_root.clone(), packaged_br()).unwrap());
    let (first, _, _) = create_workspace(&host, "First");
    let (second, _, _) = create_workspace(&host, "Second");
    let server = host.clone().start_server().await.unwrap();
    let endpoint = server.endpoint();
    let first_info = host
        .call("connection_info", json!({"workspace_id":first}))
        .unwrap();
    let second_info = host
        .call("connection_info", json!({"workspace_id":second}))
        .unwrap();
    let first_token = first_info["token"].as_str().unwrap();
    let second_token = second_info["token"].as_str().unwrap();

    assert_eq!(mcp_status(endpoint, &first, None).await, 401);
    assert_eq!(mcp_status(endpoint, &first, Some(second_token)).await, 401);
    assert_eq!(mcp_status(endpoint, &first, Some(first_token)).await, 200);

    let rotated = host
        .call("rotate_token", json!({"workspace_id":first}))
        .unwrap();
    let new_token = rotated["token"].as_str().unwrap();
    assert_eq!(mcp_status(endpoint, &first, Some(first_token)).await, 401);
    assert_eq!(mcp_status(endpoint, &first, Some(new_token)).await, 200);

    host.call("workspace_archive", json!({"workspace_id":first}))
        .unwrap();
    assert_eq!(mcp_status(endpoint, &first, Some(new_token)).await, 404);
    server.shutdown().await.unwrap();
    drop(host);

    let reopened = Arc::new(WorkspaceHost::open(data_root, packaged_br()).unwrap());
    let restarted = reopened.clone().start_server().await.unwrap();
    assert_eq!(restarted.endpoint().port(), endpoint.port());
    let list = reopened.call("workspace_list", json!({})).unwrap();
    assert_eq!(list["workspaces"].as_array().unwrap().len(), 2);
    assert_eq!(
        mcp_status(restarted.endpoint(), &second, Some(second_token)).await,
        200
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn combined_mcp_clients_discover_tasks_and_complete_a_mail_handoff() {
    let temp = TempDir::new().unwrap();
    let host = Arc::new(WorkspaceHost::open(temp.path().join("data"), packaged_br()).unwrap());
    let (workspace_id, _, _) = create_workspace(&host, "Combined MCP");
    let (other_workspace_id, _, _) = create_workspace(&host, "Other workspace");
    let server = host.clone().start_server().await.unwrap();
    let connection = host
        .call("connection_info", json!({"workspace_id":workspace_id}))
        .unwrap();
    let token = connection["token"].as_str().unwrap();
    let uri = format!("http://{}/workspaces/{workspace_id}/mcp", server.endpoint());
    let transport_a = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(uri.clone()).auth_header(token),
    );
    let transport_b = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(uri).auth_header(token),
    );
    let (client_a, client_b) = tokio::join!(().serve(transport_a), ().serve(transport_b));
    let client_a = client_a.unwrap();
    let client_b = client_b.unwrap();

    let tools = client_a.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|tool| tool.name == "workspace_info"));
    assert!(tools.iter().any(|tool| tool.name == "task_create"));
    assert!(!tools.iter().any(|tool| tool.name == "workspace_archive"));

    let discovered = client_a
        .call_tool(mcp_call("workspace_info", json!({})))
        .await
        .unwrap();
    assert_eq!(discovered.is_error, Some(false));
    let discovered = discovered.structured_content.unwrap();
    assert_eq!(discovered["workspace"]["id"], workspace_id);
    let store_id = discovered["workspace"]["task_stores"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    for (client, participant_id, name, request_id) in [
        (&client_a, "alice", "Alice", "mcp-register-alice"),
        (&client_b, "bob", "Bob", "mcp-register-bob"),
    ] {
        let registered = client
            .call_tool(mcp_call(
                "mail_register",
                json!({
                    "request_id":request_id,
                    "participant_id":participant_id,
                    "name":name
                }),
            ))
            .await
            .unwrap();
        assert_eq!(registered.is_error, Some(false));
    }

    let created = client_a
        .call_tool(mcp_call(
            "task_create",
            json!({
                "store_id":store_id,
                "request_id":"mcp-task-create",
                "title":"Review the handoff",
                "description":"Created through the combined MCP endpoint"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.is_error, Some(false));
    let created = created.structured_content.unwrap();
    let task_id = created["task"]["id"].as_str().unwrap().to_owned();
    let task_ref = json!({
        "type":"task",
        "store_id":store_id,
        "task_id":task_id
    });

    let sent = client_a
        .call_tool(mcp_call(
            "mail_send",
            json!({
                "request_id":"mcp-send-handoff",
                "sender_id":"alice",
                "destination":{"kind":"direct","id":"bob"},
                "body":"Please take this task next.",
                "kind":"handoff",
                "refs":[task_ref]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(sent.is_error, Some(false));
    let message_id = sent.structured_content.unwrap()["message"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let inbox = client_b
        .call_tool(mcp_call("mail_inbox", json!({"participant_id":"bob"})))
        .await
        .unwrap();
    assert_eq!(inbox.is_error, Some(false));
    let inbox = inbox.structured_content.unwrap();
    let delivered = inbox["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["message"]["id"] == message_id)
        .unwrap();
    assert_eq!(delivered["acknowledged"], false);
    assert_eq!(delivered["message"]["refs"][0]["task_id"], task_id);

    let acknowledged = client_b
        .call_tool(mcp_call(
            "mail_acknowledge",
            json!({
                "request_id":"mcp-ack-handoff",
                "participant_id":"bob",
                "message_ids":[message_id]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(acknowledged.is_error, Some(false));

    let replied = client_b
        .call_tool(mcp_call(
            "mail_send",
            json!({
                "request_id":"mcp-reply-handoff",
                "sender_id":"bob",
                "destination":{"kind":"direct","id":"alice"},
                "body":"Acknowledged; I have the task.",
                "kind":"handoff_reply",
                "thread_id":message_id,
                "refs":[task_ref]
            }),
        ))
        .await
        .unwrap();
    assert_eq!(replied.is_error, Some(false));

    let shown = client_b
        .call_tool(mcp_call(
            "task_show",
            json!({"store_id":store_id,"task_id":task_id}),
        ))
        .await
        .unwrap();
    assert_eq!(shown.is_error, Some(false));
    assert_eq!(
        shown.structured_content.unwrap()["task_ref"]["task_id"],
        task_id
    );
    let dependencies = client_a
        .call_tool(mcp_call(
            "task_dependencies",
            json!({"store_id":store_id,"task_id":task_id}),
        ))
        .await
        .unwrap();
    assert_eq!(dependencies.is_error, Some(false));
    assert_eq!(
        dependencies.structured_content.unwrap()["task_ref"]["store_id"],
        store_id
    );

    let spoofed = client_a
        .call_tool(mcp_call(
            "workspace_info",
            json!({"workspace_id":other_workspace_id}),
        ))
        .await
        .unwrap();
    assert_eq!(spoofed.is_error, Some(false));
    assert_eq!(
        spoofed.structured_content.unwrap()["workspace"]["id"],
        workspace_id
    );

    let admin = client_a
        .call_tool(mcp_call(
            "workspace_archive",
            json!({"workspace_id":workspace_id}),
        ))
        .await;
    match admin {
        Ok(result) => assert_eq!(result.is_error, Some(true)),
        Err(error) => assert!(error.to_string().contains("workspace_archive")),
    }

    tokio::time::timeout(std::time::Duration::from_secs(4), server.shutdown())
        .await
        .expect("server shutdown remained bounded with active rmcp clients")
        .unwrap();
    drop(client_a);
    drop(client_b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_port_replaces_persisted_port_and_is_reused() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let first = Arc::new(WorkspaceHost::open(data_root.clone(), packaged_br()).unwrap());
    let first_server = first.clone().start_server().await.unwrap();
    let first_port = first_server.endpoint().port();
    first_server.shutdown().await.unwrap();
    drop(first);

    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let replacement_port = reservation.local_addr().unwrap().port();
    assert_ne!(replacement_port, first_port);
    drop(reservation);

    let replacement = Arc::new(
        WorkspaceHost::open_with_port(data_root.clone(), packaged_br(), Some(replacement_port))
            .unwrap(),
    );
    let replacement_server = replacement.clone().start_server().await.unwrap();
    assert_eq!(replacement_server.endpoint().port(), replacement_port);
    replacement_server.shutdown().await.unwrap();
    drop(replacement);

    let reopened = Arc::new(WorkspaceHost::open(data_root, packaged_br()).unwrap());
    let restarted = reopened.clone().start_server().await.unwrap();
    assert_eq!(restarted.endpoint().port(), replacement_port);
    restarted.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn browser_api_requires_same_origin_owner_session_and_caps_bodies() {
    let temp = TempDir::new().unwrap();
    let host = Arc::new(WorkspaceHost::open(temp.path().join("data"), packaged_br()).unwrap());
    create_workspace(&host, "Browser");
    let server = host.clone().start_server().await.unwrap();
    let bootstrap = host.owner_bootstrap().unwrap();
    let origin = format!("http://{}", bootstrap.endpoint);
    let client = reqwest::Client::new();

    let anonymous = client
        .get(format!("{origin}/api/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 200);
    assert_eq!(
        anonymous.json::<serde_json::Value>().await.unwrap()["authenticated"],
        false
    );

    let missing_origin = client
        .post(format!("{origin}/api/session"))
        .json(&json!({"token":bootstrap.token}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_origin.status(), 403);
    let foreign_origin = client
        .post(format!("{origin}/api/session"))
        .header("origin", "http://example.com")
        .json(&json!({"token":bootstrap.token}))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign_origin.status(), 403);

    let login = client
        .post(format!("{origin}/api/session"))
        .header("origin", &origin)
        .json(&json!({"token":bootstrap.token}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert!(login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .contains("HttpOnly"));
    assert!(login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .contains("SameSite=Strict"));

    let call = client
        .post(format!("{origin}/api/call"))
        .header("origin", &origin)
        .header("cookie", &cookie)
        .json(&json!({"operation":"workspace_list","args":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(call.status(), 200);
    assert_eq!(
        call.json::<serde_json::Value>().await.unwrap()["result"]["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let oversized = client
        .post(format!("{origin}/api/call"))
        .header("origin", &origin)
        .header("cookie", &cookie)
        .header("content-type", "application/json")
        .body(format!(
            "{{\"operation\":\"workspace_list\",\"args\":{{\"padding\":\"{}\"}}}}",
            "x".repeat(1024 * 1024)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), 413);

    let logout = client
        .delete(format!("{origin}/api/session"))
        .header("origin", &origin)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200);
    server.shutdown().await.unwrap();
}

#[test]
fn closed_host_backup_restore_preserves_credentials_and_missing_repo_errors() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let repository = temp.path().join("repository");
    git2::Repository::init(&repository).unwrap();
    let workspace_id;
    {
        let host = WorkspaceHost::open(data_root.clone(), packaged_br()).unwrap();
        let (id, _, _) = create_workspace(&host, "Backup");
        workspace_id = id;
        host.call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":repository}),
        )
        .unwrap();
    }

    let backup = temp.path().join("backup");
    copy_tree(&data_root, &backup);
    fs::remove_dir_all(&data_root).unwrap();
    copy_tree(&backup, &data_root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let token = fs::read_dir(data_root.join("credentials"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::metadata(token).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    fs::remove_dir_all(&repository).unwrap();
    let host = WorkspaceHost::open(data_root, packaged_br()).unwrap();
    let snapshot = host
        .call(
            "workspace_snapshot",
            json!({"workspace_id":workspace_id,"history_limit":20}),
        )
        .unwrap();
    assert!(snapshot["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error["source"] == "repository"));
    assert_eq!(
        snapshot["mail"]["participants"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn lost_create_response_is_reconciled_without_a_second_task() {
    let temp = TempDir::new().unwrap();
    let wrapper = temp.path().join("br-wrapper");
    let marker = temp.path().join("failed-once");
    let source = format!(
        "#!/bin/sh\nreal='{}'\nmarker='{}'\ncase \" $* \" in\n  *' create '*)\n    if [ ! -f \"$marker\" ]; then\n      : > \"$marker\"\n      \"$real\" \"$@\" >/dev/null 2>/dev/null\n      exit 9\n    fi\n  ;;\nesac\nexec \"$real\" \"$@\"\n",
        packaged_br().display(),
        marker.display()
    );
    fs::write(&wrapper, source).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let host = WorkspaceHost::open(temp.path().join("data"), wrapper).unwrap();
    let (workspace_id, store_id, _) = create_workspace(&host, "Retry");
    let result = host
        .call(
            "task_create",
            json!({
                "workspace_id":workspace_id,"store_id":store_id,
                "request_id":"lost-response","title":"Only once"
            }),
        )
        .unwrap();
    assert_eq!(result["application_status"], "observed_after_unknown");
    let receipts = host
        .call(
            "mail_history",
            json!({
                "workspace_id":workspace_id,
                "channel_id":"task-receipts",
                "latest":true,
                "limit":20
            }),
        )
        .unwrap();
    let result_receipt = receipts["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["kind"] == "task_result")
        .unwrap();
    assert_eq!(
        result_receipt["refs"][0]["outcome"],
        "observed_after_unknown"
    );
    assert!(!result_receipt["refs"][0]["error"]
        .as_str()
        .unwrap()
        .is_empty());
    let list = host
        .call(
            "tasks_list",
            json!({"workspace_id":workspace_id,"store_id":store_id}),
        )
        .unwrap();
    assert_eq!(list["tasks"].as_array().unwrap().len(), 1);
    let replay = host
        .call(
            "task_create",
            json!({
                "workspace_id":workspace_id,"store_id":store_id,
                "request_id":"lost-response","title":"Only once"
            }),
        )
        .unwrap();
    assert_eq!(replay["idempotent_replay"], true);
}

#[test]
fn concurrent_identical_update_invokes_br_once() {
    let temp = TempDir::new().unwrap();
    let wrapper = temp.path().join("br-wrapper");
    let counter = temp.path().join("update-count");
    let source = format!(
        "#!/bin/sh\nreal='{}'\ncounter='{}'\ncase \" $* \" in\n  *' update '*) printf 'x\\n' >> \"$counter\" ;;\nesac\nexec \"$real\" \"$@\"\n",
        packaged_br().display(),
        counter.display()
    );
    write_executable(&wrapper, &source);
    let host = Arc::new(WorkspaceHost::open(temp.path().join("data"), wrapper).unwrap());
    let (workspace_id, store_id, _) = create_workspace(&host, "Concurrent");
    let task = host
        .call(
            "task_create",
            json!({"workspace_id":workspace_id,"store_id":store_id,"request_id":"c1","title":"Before"}),
        )
        .unwrap();
    let task_id = task["task"]["id"].as_str().unwrap().to_owned();
    let mut threads = Vec::new();
    for _ in 0..2 {
        let host = host.clone();
        let workspace_id = workspace_id.clone();
        let store_id = store_id.clone();
        let task_id = task_id.clone();
        threads.push(std::thread::spawn(move || {
            host.call(
                "task_update",
                json!({
                    "workspace_id":workspace_id,"store_id":store_id,"task_id":task_id,
                    "request_id":"same-update","title":"After"
                }),
            )
        }));
    }
    for thread in threads {
        thread.join().unwrap().unwrap();
    }
    assert_eq!(fs::read_to_string(counter).unwrap().lines().count(), 1);
}

#[test]
fn pending_unknown_intent_is_reconcile_only_on_retry() {
    let temp = TempDir::new().unwrap();
    let wrapper = temp.path().join("br-wrapper");
    let counter = temp.path().join("create-count");
    let source = format!(
        "#!/bin/sh\nreal='{}'\ncounter='{}'\ncase \" $* \" in\n  *' create '*) printf 'x\\n' >> \"$counter\"; exit 9 ;;\nesac\nexec \"$real\" \"$@\"\n",
        packaged_br().display(),
        counter.display()
    );
    write_executable(&wrapper, &source);
    let host = WorkspaceHost::open(temp.path().join("data"), wrapper).unwrap();
    let (workspace_id, store_id, _) = create_workspace(&host, "Pending");
    let args = json!({
        "workspace_id":workspace_id,"store_id":store_id,
        "request_id":"never-applied","title":"Do not retry"
    });
    assert!(host
        .call("task_create", args.clone())
        .unwrap_err()
        .contains("unknown"));
    assert!(host
        .call("task_create", args)
        .unwrap_err()
        .contains("will not rerun it automatically"));
    assert_eq!(fs::read_to_string(counter).unwrap().lines().count(), 1);
}

#[test]
fn external_jsonl_race_is_not_overwritten_by_flush() {
    let temp = TempDir::new().unwrap();
    let wrapper = temp.path().join("br-wrapper");
    let marker = temp.path().join("race-once");
    let source = format!(
        "#!/bin/sh\nreal='{}'\nmarker='{}'\ncase \" $* \" in\n  *' create '*)\n    if [ ! -f \"$marker\" ]; then\n      : > \"$marker\"\n      \"$real\" \"$@\"\n      status=$?\n      if [ \"$status\" -eq 0 ]; then printf '{{\"external_race\":true}}\\n' >> \"$PWD/.beads/issues.jsonl\"; fi\n      exit \"$status\"\n    fi\n  ;;\nesac\nexec \"$real\" \"$@\"\n",
        packaged_br().display(),
        marker.display()
    );
    write_executable(&wrapper, &source);
    let host = WorkspaceHost::open(temp.path().join("data"), wrapper).unwrap();
    let (workspace_id, store_id, root) = create_workspace(&host, "Race");
    let result = host
        .call(
            "task_create",
            json!({
                "workspace_id":workspace_id,"store_id":store_id,
                "request_id":"race-create","title":"Created in DB"
            }),
        )
        .unwrap();
    assert_eq!(result["application_status"], "observed_after_unknown");
    assert!(result["persistence_warning"]
        .as_str()
        .unwrap()
        .contains("external JSONL changed"));
    let jsonl = fs::read_to_string(root.join("tasks/.beads/issues.jsonl")).unwrap();
    assert!(jsonl.contains("external_race"));
}

async fn mcp_status(endpoint: SocketAddr, workspace_id: &str, token: Option<&str>) -> u16 {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!("http://{endpoint}/workspaces/{workspace_id}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .json(&json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"host-test","version":"1"}}
        }));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    request.send().await.unwrap().status().as_u16()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    #[cfg(unix)]
    fs::set_permissions(destination, fs::metadata(source).unwrap().permissions()).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            #[cfg(unix)]
            fs::set_permissions(&target, fs::metadata(entry.path()).unwrap().permissions())
                .unwrap();
        }
    }
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn second_host_cannot_open_same_data_root() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("data");
    let first = WorkspaceHost::open(root.clone(), packaged_br()).unwrap();
    let second = WorkspaceHost::open(root, packaged_br());
    assert!(matches!(second, Err(HostError::AlreadyRunning)));
    drop(first);
}
