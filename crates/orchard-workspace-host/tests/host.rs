use base64::Engine;
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
use sha2::{Digest, Sha256};
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

fn query_url(base: &str, pairs: &[(&str, &str)]) -> String {
    let encode = |value: &str| {
        value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                    char::from(byte).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect::<String>()
    };
    format!(
        "{base}?{}",
        pairs
            .iter()
            .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
            .collect::<Vec<_>>()
            .join("&")
    )
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
fn workspace_recency_is_bounded_validated_and_persists() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = WorkspaceHost::open(data_root.clone(), temp.path().join("missing-br")).unwrap();
    let first = host
        .call("workspace_create", json!({"name":"First"}))
        .unwrap()["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let second = host
        .call("workspace_create", json!({"name":"Second"}))
        .unwrap()["workspace"]["id"]
        .as_str()
        .unwrap()
        .to_owned();

    assert_eq!(
        host.call("workspace_list", json!({})).unwrap()["recent_workspace_ids"],
        json!([second, first])
    );
    host.call("workspace_visit", json!({"workspace_id":first}))
        .unwrap();
    host.call("workspace_visit", json!({"workspace_id":first}))
        .unwrap();
    assert_eq!(
        host.call("workspace_list", json!({})).unwrap()["recent_workspace_ids"],
        json!([first, second])
    );
    assert!(host
        .call("workspace_visit", json!({"workspace_id":"missing"}))
        .unwrap_err()
        .contains("unknown workspace"));

    host.call("workspace_archive", json!({"workspace_id":first}))
        .unwrap();
    assert!(host
        .call("workspace_visit", json!({"workspace_id":first}))
        .unwrap_err()
        .contains("archived"));
    assert_eq!(
        host.call("workspace_list", json!({})).unwrap()["recent_workspace_ids"],
        json!([second])
    );
    drop(host);

    let reopened = WorkspaceHost::open(data_root, temp.path().join("missing-br")).unwrap();
    assert_eq!(
        reopened.call("workspace_list", json!({})).unwrap()["recent_workspace_ids"],
        json!([second])
    );
    for index in 0..21 {
        reopened
            .call(
                "workspace_create",
                json!({"name":format!("Bounded {index}")}),
            )
            .unwrap();
    }
    assert_eq!(
        reopened.call("workspace_list", json!({})).unwrap()["recent_workspace_ids"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
}

#[test]
fn legacy_config_without_workspace_recency_still_opens() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    fs::create_dir_all(&data_root).unwrap();
    fs::write(
        data_root.join("config.json"),
        "{\"version\":1,\"port\":null,\"workspaces\":[]}",
    )
    .unwrap();
    let host = WorkspaceHost::open(data_root, temp.path().join("missing-br")).unwrap();
    assert_eq!(
        host.call("workspace_list", json!({})).unwrap()["recent_workspace_ids"],
        json!([])
    );
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
    let mut oldest_message = String::new();
    for index in 0..205 {
        let sent = host
            .call(
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
        if index == 0 {
            oldest_message = sent["message"]["id"].as_str().unwrap().to_owned();
        }
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
    let resource = host
        .call(
            "resource_get",
            json!({
                "workspace_id":workspace_id,
                "ref":{"kind":"message","workspace_id":workspace_id,"id":oldest_message}
            }),
        )
        .unwrap();
    assert_eq!(resource["resource"]["data"]["message"]["body"], "message-0");

    let empty_alerts = host
        .call(
            "workspace_alerts",
            json!({"workspace_id":workspace_id,"participant_id":"orchard","after":0,"limit":1}),
        )
        .unwrap();
    assert!(empty_alerts["alerts"].as_array().unwrap().is_empty());
    assert_eq!(empty_alerts["next_cursor"], 205);
    assert_eq!(empty_alerts["has_more"], false);

    for (request_id, body) in [
        ("alert-near", "hello @orchardish"),
        ("alert-code", "`@orchard`"),
        ("alert-mention", "hello @orchard!"),
    ] {
        host.call(
            "mail_send",
            json!({"workspace_id":workspace_id,"request_id":request_id,"sender_id":"owner","destination":{"kind":"channel","id":"general"},"body":body}),
        )
        .unwrap();
    }
    let direct = host
        .call(
            "mail_send",
            json!({"workspace_id":workspace_id,"request_id":"alert-direct","sender_id":"owner","destination":{"kind":"direct","id":"orchard"},"body":"direct"}),
        )
        .unwrap();
    let root_message = host
        .call(
            "mail_send",
            json!({"workspace_id":workspace_id,"request_id":"alert-root","sender_id":"orchard","destination":{"kind":"channel","id":"general"},"body":"root"}),
        )
        .unwrap();
    host.call(
        "mail_send",
        json!({"workspace_id":workspace_id,"request_id":"alert-reply","sender_id":"owner","destination":{"kind":"channel","id":"general"},"body":"reply","thread_id":root_message["message"]["id"]}),
    )
    .unwrap();
    host.call(
        "mail_send",
        json!({"workspace_id":workspace_id,"request_id":"alert-broadcast","sender_id":"owner","destination":{"kind":"broadcast"},"body":"broadcast"}),
    )
    .unwrap();
    host.call(
        "mail_send",
        json!({"workspace_id":workspace_id,"request_id":"alert-channel","sender_id":"owner","destination":{"kind":"channel","id":"general"},"body":"ordinary"}),
    )
    .unwrap();
    let first_alerts = host
        .call(
            "workspace_alerts",
            json!({"workspace_id":workspace_id,"participant_id":"orchard","after":205,"limit":2}),
        )
        .unwrap();
    let first = first_alerts["alerts"].as_array().unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(first[0]["reasons"], json!(["mention"]));
    assert_eq!(first[1]["reasons"], json!(["direct"]));
    assert_eq!(first_alerts["has_more"], true);
    let cursor = first_alerts["next_cursor"].as_u64().unwrap();
    let remaining = host
        .call(
            "workspace_alerts",
            json!({"workspace_id":workspace_id,"participant_id":"orchard","after":cursor}),
        )
        .unwrap();
    let remaining = remaining["alerts"].as_array().unwrap();
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0]["reasons"], json!(["reply"]));
    assert_eq!(remaining[1]["reasons"], json!(["broadcast"]));
    let channel = host
        .call(
            "workspace_alerts",
            json!({"workspace_id":workspace_id,"participant_id":"orchard","after":212,"include_channel_messages":true}),
        )
        .unwrap();
    assert_eq!(channel["alerts"][0]["reasons"], json!(["channel"]));
    let ack_args = json!({
        "workspace_id":workspace_id,"request_id":"alert-ack","participant_id":"orchard",
        "message_ids":[direct["message"]["id"].as_str().unwrap()]
    });
    assert_eq!(
        host.call("mail_acknowledge", ack_args.clone()).unwrap(),
        host.call("mail_acknowledge", ack_args).unwrap()
    );
}

#[test]
fn workspace_intro_seeds_and_preserves_readme_and_reports_paths() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = WorkspaceHost::open(data_root.clone(), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Introductions"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap().to_owned();
    let root = PathBuf::from(created["workspace"]["root"].as_str().unwrap());
    let intro = host
        .call("workspace_intro", json!({"workspace_id":workspace_id}))
        .unwrap();
    assert_eq!(intro["readme"]["exists"], true);
    assert!(intro["readme"]["text"]
        .as_str()
        .unwrap()
        .contains("## Goals"));
    assert!(intro["introduction"]
        .as_str()
        .unwrap()
        .contains("Current channels"));
    let joining_prompt = intro["joining_prompt"].as_str().unwrap();
    let credential_path = data_root
        .join("credentials")
        .join(format!("{workspace_id}.token"));
    let credential = fs::read_to_string(&credential_path).unwrap();
    assert!(joining_prompt.contains(&credential_path.to_string_lossy().to_string()));
    assert!(!joining_prompt.contains(credential.trim()));
    assert!(joining_prompt.contains(&format!("/workspaces/{workspace_id}/mcp")));
    assert!(joining_prompt
        .starts_with("I authorize you to join this workspace and make one bounded contribution"));
    for expected in [
        "capabilities",
        "tools/list",
        "workspace_intro",
        "workspace_status",
        "tasks_list",
        "task_update",
        "mail_send",
        "mail_history",
        "workspace_alerts",
        "mail_acknowledge",
        "one task or one review pass",
        "do not poll indefinitely",
        "normal harness",
        "provider permissions",
        "why you stopped",
        "no suitable authorized work",
    ] {
        assert!(joining_prompt.contains(expected), "missing {expected:?}");
    }
    assert!(!joining_prompt.contains("Do not automatically execute"));
    let info = host
        .call("workspace_info", json!({"workspace_id":workspace_id}))
        .unwrap();
    assert_eq!(
        info["paths"]["readme"],
        root.join("artifacts/README.md").to_string_lossy().as_ref()
    );
    let roots = host
        .call("artifact_roots", json!({"workspace_id":workspace_id}))
        .unwrap();
    assert_eq!(roots["roots"][0]["writable"], true);
    fs::write(root.join("artifacts/README.md"), "# Human context\n").unwrap();
    drop(host);

    let reopened = WorkspaceHost::open(data_root, temp.path().join("missing-br")).unwrap();
    let preserved = reopened
        .call("workspace_intro", json!({"workspace_id":workspace_id}))
        .unwrap();
    assert_eq!(preserved["readme"]["text"], "# Human context\n");
}

#[test]
fn resource_links_are_bidirectional_idempotent_and_persisted() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = WorkspaceHost::open(data_root.clone(), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Links"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap().to_owned();
    let source = json!({"kind":"channel","workspace_id":workspace_id,"id":"general"});
    let target =
        json!({"kind":"url","workspace_id":workspace_id,"url":"https://example.com/reference"});
    let args = json!({
        "workspace_id":workspace_id,"source":source,"target":target,
        "label":"Context","request_id":"link-one"
    });
    let first = host.call("resource_link", args.clone()).unwrap();
    assert_eq!(host.call("resource_link", args).unwrap(), first);
    let conflict = host.call(
        "resource_link",
        json!({
            "workspace_id":workspace_id,"source":source,
            "target":{"kind":"url","workspace_id":workspace_id,"url":"https://example.com/other"},
            "request_id":"link-one"
        }),
    );
    assert!(conflict.is_err());
    let outgoing = host
        .call(
            "resource_links",
            json!({"workspace_id":workspace_id,"ref":source}),
        )
        .unwrap();
    assert_eq!(outgoing["outgoing"].as_array().unwrap().len(), 1);
    let incoming = host
        .call(
            "resource_links",
            json!({"workspace_id":workspace_id,"ref":target}),
        )
        .unwrap();
    assert_eq!(incoming["incoming"].as_array().unwrap().len(), 1);
    drop(host);

    let reopened = WorkspaceHost::open(data_root, temp.path().join("missing-br")).unwrap();
    let persisted = reopened
        .call(
            "resource_links",
            json!({"workspace_id":workspace_id,"ref":target}),
        )
        .unwrap();
    assert_eq!(persisted["incoming"].as_array().unwrap().len(), 1);
}

#[test]
fn owned_artifact_upload_retries_original_revision_and_reads_exact_versions() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = WorkspaceHost::open(data_root.clone(), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Artifacts"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap().to_owned();
    let workspace_root = PathBuf::from(created["workspace"]["root"].as_str().unwrap());
    let encode = |value: &[u8]| base64::engine::general_purpose::STANDARD.encode(value);
    let artifact_root = workspace_root.join("artifacts");
    fs::create_dir_all(artifact_root.join(".orchard/requests")).unwrap();
    git2::Repository::init(&artifact_root).unwrap();
    fs::write(
        artifact_root.join(".orchard/requests/recover-one.json"),
        serde_json::to_vec(&json!({
            "path":"recovered.txt",
            "fingerprint":format!("{:x}", Sha256::digest(b"recovered"))
        }))
        .unwrap(),
    )
    .unwrap();
    let recovered = host
        .call(
            "artifact_upload",
            json!({
                "workspace_id":workspace_id,"path":"recovered.txt",
                "content_base64":encode(b"recovered"),"request_id":"recover-one"
            }),
        )
        .unwrap();
    assert_eq!(recovered["resource"]["ref"]["path"], "recovered.txt");
    let first_args = json!({
        "workspace_id":workspace_id,"path":"notes/example.md",
        "content_base64":encode(b"first version"),"request_id":"upload-one"
    });
    let first = host.call("artifact_upload", first_args.clone()).unwrap();
    let first_revision = first["resource"]["ref"]["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    host.call(
        "artifact_upload",
        json!({
            "workspace_id":workspace_id,"path":"other.txt",
            "content_base64":encode(b"unrelated"),"request_id":"upload-two"
        }),
    )
    .unwrap();
    let replay = host.call("artifact_upload", first_args).unwrap();
    assert_eq!(replay["resource"]["ref"]["revision"], first_revision);
    assert!(host
        .call(
            "artifact_upload",
            json!({
                "workspace_id":workspace_id,"path":"notes/example.md",
                "content_base64":encode(b"different"),"request_id":"upload-one"
            }),
        )
        .unwrap_err()
        .contains("different upload"));
    let latest = host
        .call(
            "artifact_upload",
            json!({
                "workspace_id":workspace_id,"path":"notes/example.md",
                "content_base64":encode(b"second version"),"request_id":"upload-three"
            }),
        )
        .unwrap();
    assert_ne!(latest["resource"]["ref"]["revision"], first_revision);
    let pinned = host
        .call(
            "resource_get",
            json!({
                "workspace_id":workspace_id,
                "ref":{"kind":"file","workspace_id":workspace_id,"root_id":"artifacts",
                    "path":"notes/example.md","revision":first_revision}
            }),
        )
        .unwrap();
    assert_eq!(pinned["resource"]["data"]["text"], "first version");
    let live = host
        .call(
            "resource_get",
            json!({
                "workspace_id":workspace_id,
                "ref":{"kind":"file","workspace_id":workspace_id,"root_id":"artifacts",
                    "path":"notes/example.md"}
            }),
        )
        .unwrap();
    assert_eq!(live["resource"]["data"]["text"], "second version");
    let history = host
        .call(
            "artifact_history",
            json!({"workspace_id":workspace_id,"root_id":"artifacts","path":"notes/example.md"}),
        )
        .unwrap();
    assert_eq!(history["versions"].as_array().unwrap().len(), 2);
    drop(host);

    let reopened = WorkspaceHost::open(data_root, temp.path().join("missing-br")).unwrap();
    let replay = reopened
        .call(
            "artifact_upload",
            json!({
                "workspace_id":workspace_id,"path":"notes/example.md",
                "content_base64":encode(b"first version"),"request_id":"upload-one"
            }),
        )
        .unwrap();
    assert_eq!(replay["resource"]["ref"]["revision"], first_revision);
}

#[test]
fn artifact_direct_commit_and_delete_are_scoped_and_idempotent() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = WorkspaceHost::open(data_root.clone(), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Artifact CRUD"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap().to_owned();
    let root = PathBuf::from(created["workspace"]["root"].as_str().unwrap()).join("artifacts");

    fs::create_dir_all(root.join("nested")).unwrap();
    fs::write(root.join("nested/local.txt"), "local one").unwrap();
    let committed = host
        .call(
            "artifact_commit",
            json!({"workspace_id":workspace_id,"paths":["nested/local.txt"],"request_id":"direct-one","message":"Add local"}),
        )
        .unwrap();
    let direct_revision = committed["revision"].as_str().unwrap().to_owned();
    assert_eq!(
        host.call(
            "resource_get",
            json!({"workspace_id":workspace_id,"ref":{"kind":"file","workspace_id":workspace_id,"root_id":"artifacts","path":"nested/local.txt"}}),
        )
        .unwrap()["resource"]["data"]["text"],
        "local one"
    );
    fs::write(root.join("nested/local.txt"), "local two").unwrap();
    fs::write(root.join("unrelated.txt"), "unrelated").unwrap();
    assert!(host
        .call(
            "artifact_commit",
            json!({"workspace_id":workspace_id,"paths":["nested/local.txt"],"request_id":"direct-two"}),
        )
        .unwrap_err()
        .contains("unrelated"));
    fs::remove_file(root.join("unrelated.txt")).unwrap();
    host.call(
        "artifact_commit",
        json!({"workspace_id":workspace_id,"paths":["nested/local.txt"],"request_id":"direct-two"}),
    )
    .unwrap();
    fs::remove_file(root.join("nested/local.txt")).unwrap();
    let local_delete = host
        .call(
            "artifact_commit",
            json!({"workspace_id":workspace_id,"paths":["nested/local.txt"],"request_id":"direct-delete"}),
        )
        .unwrap();
    assert_eq!(local_delete["committed"], true);

    let uploaded = host
        .call(
            "artifact_upload",
            json!({
                "workspace_id":workspace_id,"path":"delete-me.txt","request_id":"delete-upload",
                "content_base64":base64::engine::general_purpose::STANDARD.encode(b"before delete")
            }),
        )
        .unwrap();
    let upload_revision = uploaded["revision"].as_str().unwrap().to_owned();
    let delete_args =
        json!({"workspace_id":workspace_id,"path":"delete-me.txt","request_id":"delete-one"});
    let deleted = host.call("artifact_delete", delete_args.clone()).unwrap();
    let deletion_revision = deleted["revision"].as_str().unwrap().to_owned();
    host.call(
        "artifact_upload",
        json!({
            "workspace_id":workspace_id,"path":"later.txt","request_id":"later-upload",
            "content_base64":base64::engine::general_purpose::STANDARD.encode(b"later")
        }),
    )
    .unwrap();
    assert_eq!(
        host.call("artifact_delete", delete_args.clone()).unwrap()["revision"],
        deletion_revision
    );
    let historical = host
        .call(
            "resource_get",
            json!({"workspace_id":workspace_id,"ref":{"kind":"file","workspace_id":workspace_id,"root_id":"artifacts","path":"delete-me.txt","revision":upload_revision}}),
        )
        .unwrap();
    assert_eq!(historical["resource"]["data"]["text"], "before delete");
    assert!(host
        .call(
            "artifact_delete",
            json!({"workspace_id":workspace_id,"path":"missing.txt","request_id":"missing-delete"}),
        )
        .unwrap_err()
        .contains("tracked file"));
    assert!(host
        .call(
            "artifact_delete",
            json!({"workspace_id":workspace_id,"path":"../escape","request_id":"unsafe-delete"}),
        )
        .is_err());
    drop(host);

    let reopened = WorkspaceHost::open(data_root, temp.path().join("missing-br")).unwrap();
    assert_eq!(
        reopened.call("artifact_delete", delete_args).unwrap()["revision"],
        deletion_revision
    );
    assert_eq!(
        reopened
            .call(
                "artifact_commit",
                json!({"workspace_id":workspace_id,"paths":["nested/local.txt"],"request_id":"direct-one","message":"Add local"}),
            )
            .unwrap()["revision"],
        direct_revision
    );
}

#[test]
fn artifact_commit_rejects_changed_staged_retry_without_mutating_index() {
    let temp = TempDir::new().unwrap();
    let host =
        WorkspaceHost::open(temp.path().join("data"), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Staged recovery"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap().to_owned();
    let root = PathBuf::from(created["workspace"]["root"].as_str().unwrap()).join("artifacts");
    let path = "staged.txt";
    fs::write(root.join(path), "base").unwrap();
    host.call(
        "artifact_commit",
        json!({"workspace_id":workspace_id,"paths":[path],"request_id":"staged-base"}),
    )
    .unwrap();

    let intended = b"intended retry contents";
    fs::write(root.join(path), intended).unwrap();
    let paths = vec![path.to_owned()];
    let request_fingerprint = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&json!({"paths":paths,"message":"Commit artifact changes"}))
                .unwrap()
        )
    );
    let receipt_path = root.join(".orchard/requests/staged-retry.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&json!({
            "operation":"commit",
            "paths":[path],
            "request_fingerprint":request_fingerprint,
            "content_fingerprints":{path:format!("{:x}", Sha256::digest(intended))}
        }))
        .unwrap(),
    )
    .unwrap();

    let repo = git2::Repository::open(&root).unwrap();
    fs::write(root.join(path), "different staged contents").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let staged_before = index.get_path(Path::new(path), 0).unwrap().id;
    fs::write(root.join(path), intended).unwrap();

    let error = host
        .call(
            "artifact_commit",
            json!({"workspace_id":workspace_id,"paths":[path],"request_id":"staged-retry"}),
        )
        .unwrap_err();
    assert!(error.contains("staged contents changed"), "{error}");
    let index = repo.index().unwrap();
    assert_eq!(
        index.get_path(Path::new(path), 0).unwrap().id,
        staged_before
    );
    assert_eq!(
        repo.find_blob(staged_before).unwrap().content(),
        b"different staged contents"
    );
}

#[test]
fn artifact_delete_rejects_changed_staged_retry_without_mutating_index() {
    let temp = TempDir::new().unwrap();
    let host =
        WorkspaceHost::open(temp.path().join("data"), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Delete recovery"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap().to_owned();
    let root = PathBuf::from(created["workspace"]["root"].as_str().unwrap()).join("artifacts");
    let path = "delete-staged.txt";
    let original = b"original delete contents";
    fs::write(root.join(path), original).unwrap();
    host.call(
        "artifact_commit",
        json!({"workspace_id":workspace_id,"paths":[path],"request_id":"delete-stage-base"}),
    )
    .unwrap();

    let repo = git2::Repository::open(&root).unwrap();
    let previous_oid = repo
        .index()
        .unwrap()
        .get_path(Path::new(path), 0)
        .unwrap()
        .id;
    fs::write(
        root.join(".orchard/requests/delete-staged-retry.json"),
        serde_json::to_vec_pretty(&json!({
            "operation":"delete","path":path,"previous_oid":previous_oid.to_string()
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(root.join(path), "different staged delete contents").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let staged_before = index.get_path(Path::new(path), 0).unwrap().id;
    fs::write(root.join(path), original).unwrap();

    let error = host
        .call(
            "artifact_delete",
            json!({"workspace_id":workspace_id,"path":path,"request_id":"delete-staged-retry"}),
        )
        .unwrap_err();
    assert!(error.contains("changed staged contents"), "{error}");
    let index = repo.index().unwrap();
    assert_eq!(
        index.get_path(Path::new(path), 0).unwrap().id,
        staged_before
    );
    assert_eq!(
        repo.find_blob(staged_before).unwrap().content(),
        b"different staged delete contents"
    );
    assert_eq!(fs::read(root.join(path)).unwrap(), original);

    let mut index = repo.index().unwrap();
    index.add_path(Path::new(path)).unwrap();
    index.remove_path(Path::new(path)).unwrap();
    index.write().unwrap();
    let recovered = host
        .call(
            "artifact_delete",
            json!({"workspace_id":workspace_id,"path":path,"request_id":"delete-staged-retry"}),
        )
        .unwrap();
    assert_eq!(recovered["deleted"], true);
    assert!(!root.join(path).exists());
}

#[cfg(unix)]
#[test]
fn owned_artifact_upload_rejects_redirected_git_metadata() {
    use std::os::unix::fs::symlink;

    let temp = TempDir::new().unwrap();
    let host =
        WorkspaceHost::open(temp.path().join("data"), temp.path().join("missing-br")).unwrap();
    let created = host
        .call("workspace_create", json!({"name":"Safe artifacts"}))
        .unwrap();
    let workspace_id = created["workspace"]["id"].as_str().unwrap();
    let workspace_root = PathBuf::from(created["workspace"]["root"].as_str().unwrap());
    let artifact_root = workspace_root.join("artifacts");
    let outside = temp.path().join("outside");
    fs::remove_dir_all(&artifact_root).unwrap();
    fs::create_dir_all(&artifact_root).unwrap();
    git2::Repository::init(&outside).unwrap();
    symlink(outside.join(".git"), artifact_root.join(".git")).unwrap();

    let error = host
        .call(
            "artifact_upload",
            json!({
                "workspace_id":workspace_id,"path":"escape.txt","request_id":"escape-one",
                "content_base64":base64::engine::general_purpose::STANDARD.encode(b"no escape")
            }),
        )
        .unwrap_err();
    assert!(error.contains(".git must be a local directory"));
    assert!(!outside.join("escape.txt").exists());
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
fn project_attach_discovers_repo_root_beads_and_lists_qualified_tasks() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let host = WorkspaceHost::open(data_root.clone(), packaged_br()).unwrap();
    let (workspace_id, owned_store_id, workspace_root) = create_workspace(&host, "Projects");
    let created = host
        .call(
            "task_create",
            json!({
                "workspace_id":workspace_id,"store_id":owned_store_id,
                "request_id":"project-fixture","title":"Project task"
            }),
        )
        .unwrap();
    let task_id = created["task"]["id"].as_str().unwrap().to_owned();

    let repository = temp.path().join("compatible-project");
    git2::Repository::init(&repository).unwrap();
    copy_tree(
        &workspace_root.join("tasks/.beads"),
        &repository.join(".beads"),
    );
    let attached = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":repository}),
        )
        .unwrap();
    assert_eq!(attached["attached"], true);
    assert_eq!(attached["task_store_attached"], true);
    assert_eq!(attached["repository"]["name"], "compatible-project");
    assert_eq!(attached["repository"]["task_status"], "linked");
    assert_eq!(attached["task_store"]["source"], "repository");
    let project_store_id = attached["repository"]["task_store_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tasks = host
        .call(
            "tasks_list",
            json!({"workspace_id":workspace_id,"store_id":project_store_id}),
        )
        .unwrap();
    let copied = tasks["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"] == task_id)
        .unwrap();
    assert_eq!(copied["task_ref"]["store_id"], project_store_id);
    assert_ne!(project_store_id, owned_store_id);

    drop(host);
    let reopened = WorkspaceHost::open(data_root.clone(), packaged_br()).unwrap();
    let workspace = &reopened.call("workspace_list", json!({})).unwrap()["workspaces"][0];
    assert_eq!(workspace["repositories"][0]["task_status"], "linked");
    assert_eq!(workspace["task_stores"][0]["source"], "owned");
    assert_eq!(workspace["task_stores"][1]["source"], "repository");

    let config_before = fs::read(data_root.join("config.json")).unwrap();
    fs::remove_file(repository.join(".beads/beads.db")).unwrap();
    let snapshot = reopened
        .call("workspace_snapshot", json!({"workspace_id":workspace_id}))
        .unwrap();
    assert_eq!(snapshot["repositories"][0]["task_status"], "missing");
    assert!(snapshot["repositories"][0]["task_error"]
        .as_str()
        .unwrap()
        .contains("is missing"));
    assert_eq!(
        fs::read(data_root.join("config.json")).unwrap(),
        config_before
    );
}

#[test]
fn project_attach_keeps_no_beads_and_unsupported_projects_visible_and_read_only() {
    let temp = TempDir::new().unwrap();
    let host = WorkspaceHost::open(temp.path().join("data"), packaged_br()).unwrap();
    let (workspace_id, _, _) = create_workspace(&host, "Project status");

    let plain = temp.path().join("plain");
    git2::Repository::init(&plain).unwrap();
    fs::write(plain.join("beads.db"), b"not a repo-root .beads store").unwrap();
    let plain_result = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":plain}),
        )
        .unwrap();
    assert_eq!(plain_result["repository"]["task_status"], "none");
    assert_eq!(plain_result["repository"]["task_store_id"], Value::Null);
    assert!(!plain.join(".beads").exists());

    let bare = temp.path().join("bare.git");
    git2::Repository::init_bare(&bare).unwrap();
    let bare_result = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":bare}),
        )
        .unwrap();
    assert_eq!(bare_result["repository"]["name"], "bare");
    assert_eq!(bare_result["repository"]["task_status"], "none");

    let unsupported = temp.path().join("unsupported-project");
    git2::Repository::init(&unsupported).unwrap();
    fs::create_dir_all(unsupported.join(".beads")).unwrap();
    let db_path = unsupported.join(".beads/beads.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "PRAGMA user_version=1; CREATE TABLE issues(id TEXT PRIMARY KEY, title TEXT); CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES ('unchanged');",
    )
    .unwrap();
    drop(db);
    let unsupported_result = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":unsupported}),
        )
        .unwrap();
    assert_eq!(
        unsupported_result["repository"]["task_status"],
        "unsupported"
    );
    assert!(unsupported_result["repository"]["task_error"]
        .as_str()
        .unwrap()
        .contains("unsupported Beads schema"));
    assert_eq!(unsupported_result["task_store"], Value::Null);
    let sentinel: String = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sentinel, "unchanged");
}

#[test]
fn project_reattach_reconciles_new_beads_and_detach_preserves_sources_and_files() {
    let temp = TempDir::new().unwrap();
    let host = WorkspaceHost::open(temp.path().join("data"), packaged_br()).unwrap();
    let (workspace_id, owned_store_id, workspace_root) = create_workspace(&host, "Reconcile");
    let repository = temp.path().join("later-beads");
    git2::Repository::init(&repository).unwrap();

    let first = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":repository}),
        )
        .unwrap();
    let repository_id = first["repository"]["id"].as_str().unwrap().to_owned();
    copy_tree(
        &workspace_root.join("tasks/.beads"),
        &repository.join(".beads"),
    );
    let second = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":repository}),
        )
        .unwrap();
    assert_eq!(second["attached"], false);
    assert_eq!(second["repository"]["id"], repository_id);
    assert_eq!(second["repository"]["task_status"], "linked");
    let store_id = second["repository"]["task_store_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let third = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":repository}),
        )
        .unwrap();
    assert_eq!(third["task_store_attached"], false);
    assert_eq!(third["repository"]["task_store_id"], store_id);

    #[cfg(unix)]
    {
        let alias = temp.path().join("project-alias");
        std::os::unix::fs::symlink(&repository, &alias).unwrap();
        let alias_result = host
            .call(
                "repository_attach",
                json!({"workspace_id":workspace_id,"path":alias}),
            )
            .unwrap();
        assert_eq!(alias_result["attached"], false);
        assert_eq!(alias_result["repository"]["id"], repository_id);
    }

    let (other_workspace_id, _, _) = create_workspace(&host, "Other workspace");
    let cross_workspace = host.call(
        "repository_attach",
        json!({"workspace_id":other_workspace_id,"path":repository}),
    );
    assert!(cross_workspace
        .unwrap_err()
        .contains("already attached to workspace"));
    let workspaces = host.call("workspace_list", json!({})).unwrap();
    assert!(workspaces["workspaces"][1]["repositories"]
        .as_array()
        .unwrap()
        .is_empty());

    let owned_detach = host.call(
        "task_store_detach",
        json!({"workspace_id":workspace_id,"store_id":owned_store_id}),
    );
    assert!(owned_detach.unwrap_err().contains("cannot be detached"));
    host.call(
        "repository_detach",
        json!({"workspace_id":workspace_id,"repository_id":repository_id}),
    )
    .unwrap();
    assert!(repository.join(".beads/beads.db").exists());
    let workspace = &host.call("workspace_list", json!({})).unwrap()["workspaces"][0];
    assert!(workspace["repositories"].as_array().unwrap().is_empty());
    let retained = workspace["task_stores"]
        .as_array()
        .unwrap()
        .iter()
        .find(|store| store["id"] == store_id)
        .unwrap();
    assert_eq!(retained["source"], "external");
    assert_eq!(retained["repository_id"], Value::Null);

    let reattached = host
        .call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":repository}),
        )
        .unwrap();
    assert_eq!(reattached["repository"]["task_store_id"], store_id);
    assert_eq!(reattached["task_store_attached"], false);
    host.call(
        "task_store_detach",
        json!({"workspace_id":workspace_id,"store_id":store_id}),
    )
    .unwrap();
    assert!(repository.join(".beads/beads.db").exists());
    let workspace = &host.call("workspace_list", json!({})).unwrap()["workspaces"][0];
    assert_eq!(workspace["repositories"][0]["task_store_id"], Value::Null);
    assert_eq!(workspace["repositories"][0]["task_status"], "none");
}

#[test]
fn legacy_config_defaults_preserve_project_and_owned_source_views() {
    let temp = TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    let repository = temp.path().join("legacy-project");
    git2::Repository::init(&repository).unwrap();
    {
        let host = WorkspaceHost::open(data_root.clone(), packaged_br()).unwrap();
        let (workspace_id, _, _) = create_workspace(&host, "Legacy config");
        host.call(
            "repository_attach",
            json!({"workspace_id":workspace_id,"path":repository}),
        )
        .unwrap();
    }

    let config_path = data_root.join("config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    let workspace = &mut config["workspaces"][0];
    for field in ["name", "task_store_id", "task_status", "task_error"] {
        workspace["repositories"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    for store in workspace["task_stores"].as_array_mut().unwrap() {
        store.as_object_mut().unwrap().remove("source");
        store.as_object_mut().unwrap().remove("repository_id");
    }
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();

    let host = WorkspaceHost::open(data_root, packaged_br()).unwrap();
    let workspace = &host.call("workspace_list", json!({})).unwrap()["workspaces"][0];
    assert_eq!(workspace["repositories"][0]["name"], "legacy-project");
    assert_eq!(workspace["repositories"][0]["task_status"], "none");
    assert_eq!(workspace["task_stores"][0]["source"], "owned");
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
    assert!(tools.iter().any(|tool| tool.name == "resource_get"));
    assert!(!tools.iter().any(|tool| tool.name == "workspace_archive"));

    let discovered = client_a
        .call_tool(mcp_call("workspace_info", json!({})))
        .await
        .unwrap();
    assert_eq!(discovered.is_error, Some(false));
    let discovered = discovered.structured_content.unwrap();
    assert_eq!(discovered["workspace"]["id"], workspace_id);
    let own_resource = client_a
        .call_tool(mcp_call(
            "resource_get",
            json!({"ref":{"kind":"url","workspace_id":workspace_id,"url":"https://example.com"}}),
        ))
        .await
        .unwrap();
    assert_eq!(own_resource.is_error, Some(false));
    let foreign_resource = client_a
        .call_tool(mcp_call(
            "resource_get",
            json!({"ref":{"kind":"url","workspace_id":other_workspace_id,"url":"https://example.com"}}),
        ))
        .await
        .unwrap();
    assert_eq!(foreign_resource.is_error, Some(true));
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
    let (workspace_id, _, _) = create_workspace(&host, "Browser");
    let (other_workspace_id, _, _) = create_workspace(&host, "Other browser");
    let server = host.clone().start_server().await.unwrap();
    let workspace_token = host
        .call("connection_info", json!({"workspace_id":workspace_id}))
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let other_token = host
        .call(
            "connection_info",
            json!({"workspace_id":other_workspace_id}),
        )
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    host.call(
        "artifact_upload",
        json!({
            "workspace_id":workspace_id,"path":"images/space ü.png","request_id":"browser-image",
            "content_base64":base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture")
        }),
    )
    .unwrap();
    let bootstrap = host.owner_bootstrap().unwrap();
    let origin = format!("http://{}", bootstrap.endpoint);
    let client = reqwest::Client::new();
    let resource_endpoint = format!("{origin}/api/workspaces/{workspace_id}/resource");
    let own_href = format!("/w/{workspace_id}/urls?url=https%3A%2F%2Fexample.com");
    assert_eq!(
        client
            .get(query_url(&resource_endpoint, &[("href", &own_href)]))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let bearer_read = client
        .get(query_url(&resource_endpoint, &[("href", &own_href)]))
        .bearer_auth(&workspace_token)
        .send()
        .await
        .unwrap();
    assert_eq!(bearer_read.status(), 200);
    assert_eq!(bearer_read.headers()["cache-control"], "private, no-store");
    assert_eq!(
        client
            .get(query_url(&resource_endpoint, &[("href", &own_href)]))
            .bearer_auth(&other_token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(query_url(&resource_endpoint, &[("href", &own_href)]))
            .bearer_auth(&workspace_token)
            .header("origin", "http://example.com")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let foreign_href = format!("/w/{other_workspace_id}/urls?url=https%3A%2F%2Fexample.com");
    assert_eq!(
        client
            .get(query_url(&resource_endpoint, &[("href", &foreign_href)]))
            .bearer_auth(&workspace_token)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );

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
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_origin.status(), 403);
    let foreign_origin = client
        .post(format!("{origin}/api/session"))
        .header("origin", "http://example.com")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign_origin.status(), 403);
    let opaque_origin = client
        .post(format!("{origin}/api/session"))
        .header("origin", "null")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(opaque_origin.status(), 403);
    let wrong_host = client
        .post(format!("{origin}/api/session"))
        .header("origin", &origin)
        .header("host", format!("localhost:{}", bootstrap.endpoint.port()))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_host.status(), 403);
    let invalid_legacy_credential = client
        .post(format!("{origin}/api/session"))
        .header("origin", &origin)
        .json(&json!({"token":"not-the-owner-credential"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_legacy_credential.status(), 401);

    let login = client
        .post(format!("{origin}/api/session"))
        .header("origin", &origin)
        .json(&json!({}))
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
    assert_eq!(
        client
            .get(format!("{origin}/api/session"))
            .header("origin", "null")
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );

    let download_endpoint = format!("{origin}/api/workspaces/{workspace_id}/artifact/download");
    let download = client
        .get(query_url(
            &download_endpoint,
            &[("root_id", "artifacts"), ("path", "images/space ü.png")],
        ))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), 200);
    assert_eq!(
        download.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(download.headers()["x-content-type-options"], "nosniff");
    assert!(download.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .contains("filename*=UTF-8''space%20%C3%BC.png"));
    let preview = client
        .get(query_url(
            &download_endpoint,
            &[
                ("root_id", "artifacts"),
                ("path", "images/space ü.png"),
                ("preview", "true"),
            ],
        ))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(preview.status(), 200);
    assert_eq!(preview.headers()["content-type"], "image/png");
    assert_eq!(preview.headers()["content-disposition"], "inline");

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
        2
    );
    assert_eq!(
        client
            .post(format!("{origin}/api/call"))
            .header("origin", "null")
            .header("cookie", &cookie)
            .json(&json!({"operation":"workspace_list","args":{}}))
            .send()
            .await
            .unwrap()
            .status(),
        403
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
    let logged_out = client
        .get(format!("{origin}/api/session"))
        .send()
        .await
        .unwrap();
    assert_eq!(logged_out.status(), 200);
    assert_eq!(
        logged_out.json::<serde_json::Value>().await.unwrap()["authenticated"],
        false
    );
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
