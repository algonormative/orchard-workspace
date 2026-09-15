use git2::{Repository, Signature};
use orchard_mail_core::{MailError, MailService};
use serde_json::{json, Value};
use std::{fs, path::Path};
use tempfile::TempDir;

fn register(mail: &mut MailService, request_id: &str, id: &str) -> Value {
    mail.call(
        "mail_register",
        json!({"request_id": request_id, "name": id, "participant_id": id}),
    )
    .unwrap()
}

fn send(
    mail: &mut MailService,
    request_id: &str,
    sender: &str,
    destination: Value,
    body: &str,
) -> Value {
    mail.call(
        "mail_send",
        json!({
            "request_id": request_id, "sender_id": sender, "destination": destination,
            "body": body, "kind": "message", "refs": []
        }),
    )
    .unwrap()
}

fn mailbox() -> (TempDir, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("mail");
    (temp, root)
}

fn commit_file(root: &Path, relative: &str, contents: &str, summary: &str) {
    fs::write(root.join(relative), contents).unwrap();
    commit_paths(root, &[relative], summary, false);
}

fn commit_paths(root: &Path, relatives: &[&str], summary: &str, orchard_identity: bool) {
    let repo = Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    for relative in relatives {
        index.add_path(Path::new(relative)).unwrap();
    }
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = if orchard_identity {
        Signature::now("Orchard Mail", "orchard-mail@localhost").unwrap()
    } else {
        Signature::now("test", "test@example.invalid").unwrap()
    };
    repo.commit(Some("HEAD"), &sig, &sig, summary, &tree, &[&parent])
        .unwrap();
}

#[test]
fn persists_retries_and_monotonic_ids_across_restart() {
    let (_temp, root) = mailbox();
    let first;
    {
        let mut mail = MailService::open(&root).unwrap();
        register(&mut mail, "register-alice", "alice");
        register(&mut mail, "register-bob", "bob");
        let args = json!({
            "request_id":"send-1", "sender_id":"alice",
            "destination":{"kind":"direct","id":"bob"}, "body":"hello",
            "kind":"message", "refs":[]
        });
        first = mail.call("mail_send", args.clone()).unwrap();
        assert_eq!(first, mail.call("mail_send", args).unwrap());
        assert!(matches!(
            mail.call(
                "mail_send",
                json!({
                    "request_id":"send-1", "sender_id":"alice",
                    "destination":{"kind":"direct","id":"bob"}, "body":"changed"
                })
            ),
            Err(MailError::RequestConflict(_))
        ));
        assert!(matches!(
            mail.call(
                "mail_leave",
                json!({"request_id":"send-1", "participant_id":"alice"})
            ),
            Err(MailError::RequestConflict(_))
        ));
    }
    let mut reopened = MailService::open(&root).unwrap();
    let second = send(
        &mut reopened,
        "send-2",
        "bob",
        json!({"kind":"broadcast"}),
        "again",
    );
    assert_eq!(first["message"]["sequence"], 1);
    assert_eq!(first["message"]["request_id"], "send-1");
    assert_eq!(second["message"]["sequence"], 2);
    assert_ne!(first["message"]["id"], second["message"]["id"]);

    let repo = Repository::open(&root).unwrap();
    let mut count = 0;
    let mut walk = repo.revwalk().unwrap();
    walk.push_head().unwrap();
    for oid in walk {
        oid.unwrap();
        count += 1;
    }
    assert_eq!(count, 5); // init, two registrations, two sends
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.author().name(), Some("Orchard Mail"));
    assert!(head
        .tree()
        .unwrap()
        .get_path(Path::new("messages/m_00000000000000000002.md"))
        .is_ok());
}

#[test]
fn snapshots_direct_channel_broadcast_and_leave_recipients() {
    let (_temp, root) = mailbox();
    let mut mail = MailService::open(&root).unwrap();
    register(&mut mail, "ra", "alice");
    register(&mut mail, "rb", "bob");

    send(
        &mut mail,
        "direct",
        "alice",
        json!({"kind":"direct","id":"bob"}),
        "direct",
    );
    send(
        &mut mail,
        "channel",
        "alice",
        json!({"kind":"channel","id":"general"}),
        "channel",
    );
    register(&mut mail, "rc", "charlie");
    let before = mail
        .call("mail_inbox", json!({"participant_id":"charlie"}))
        .unwrap();
    assert_eq!(before["messages"].as_array().unwrap().len(), 0);

    send(
        &mut mail,
        "broadcast",
        "alice",
        json!({"kind":"broadcast"}),
        "broadcast",
    );
    mail.call(
        "mail_leave",
        json!({"request_id":"leave-bob", "participant_id":"bob"}),
    )
    .unwrap();
    send(
        &mut mail,
        "after-leave",
        "alice",
        json!({"kind":"channel","id":"general"}),
        "later",
    );

    let bob = mail
        .call("mail_inbox", json!({"participant_id":"bob","limit":200}))
        .unwrap();
    let bob_bodies: Vec<_> = bob["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message"]["body"].as_str().unwrap())
        .collect();
    assert_eq!(bob_bodies, ["direct", "channel", "broadcast"]);
    let charlie = mail
        .call(
            "mail_inbox",
            json!({"participant_id":"charlie","limit":200}),
        )
        .unwrap();
    let charlie_bodies: Vec<_> = charlie["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message"]["body"].as_str().unwrap())
        .collect();
    assert_eq!(charlie_bodies, ["broadcast", "later"]);
    let direct = mail
        .call(
            "mail_history",
            json!({"destination_kind":"direct","latest":true}),
        )
        .unwrap();
    assert_eq!(direct["messages"].as_array().unwrap().len(), 1);
    assert_eq!(direct["messages"][0]["body"], "direct");
}

#[test]
fn acknowledgement_persists_and_resume_presence_does_not() {
    let (_temp, root) = mailbox();
    let id;
    {
        let mut mail = MailService::open(&root).unwrap();
        register(&mut mail, "ra", "alice");
        register(&mut mail, "rb", "bob");
        let message = send(
            &mut mail,
            "send",
            "alice",
            json!({"kind":"direct","id":"bob"}),
            "hello",
        );
        id = message["message"]["id"].as_str().unwrap().to_owned();
        let resumed = mail
            .call("mail_resume", json!({"participant_id":"bob"}))
            .unwrap();
        assert!(resumed["participant"]["session_instance_id"]
            .as_str()
            .unwrap()
            .starts_with("s_"));
        mail.call(
            "mail_acknowledge",
            json!({"request_id":"ack", "participant_id":"bob", "message_ids":[id]}),
        )
        .unwrap();
    }
    let mut mail = MailService::open(&root).unwrap();
    let participants = mail.call("mail_participants", json!({})).unwrap();
    let bob = participants["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "bob")
        .unwrap();
    assert!(bob.get("session_instance_id").is_none());
    assert!(bob.get("last_contact_at").is_none());
    let inbox = mail
        .call("mail_inbox", json!({"participant_id":"bob"}))
        .unwrap();
    assert_eq!(inbox["messages"][0]["acknowledged"], true);
}

#[test]
fn exclusive_writer_lock_is_enforced() {
    let (_temp, root) = mailbox();
    let _first = MailService::open(&root).unwrap();
    assert!(matches!(
        MailService::open(&root),
        Err(MailError::Locked(_))
    ));
}

#[test]
fn unexpected_dirty_and_committed_layout_corruption_are_refused() {
    let (_temp, root) = mailbox();
    drop(MailService::open(&root).unwrap());
    fs::write(root.join("unexpected.txt"), "do not touch").unwrap();
    assert!(matches!(MailService::open(&root), Err(MailError::Dirty(_))));
    assert_eq!(
        fs::read_to_string(root.join("unexpected.txt")).unwrap(),
        "do not touch"
    );
    fs::remove_file(root.join("unexpected.txt")).unwrap();
    commit_file(&root, "unexpected.md", "unexpected", "inject corruption");
    assert!(matches!(
        MailService::open(&root),
        Err(MailError::Corrupt(_))
    ));
}

#[test]
fn recovers_only_owned_staged_paths_and_never_commits_them() {
    let (_temp, root) = mailbox();
    drop(MailService::open(&root).unwrap());
    let repo = Repository::open(&root).unwrap();
    let head = repo.head().unwrap().target().unwrap();
    let original = fs::read_to_string(root.join("state.md")).unwrap();
    fs::write(root.join("state.md"), "partial write").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("state.md")).unwrap();
    index.write().unwrap();
    fs::write(repo.path().join("orchard-mail-transaction.json"), serde_json::to_vec_pretty(&json!({
        "head":head.to_string(), "paths":["state.md"], "summary":"mail_send crash", "expected_tree":null
    })).unwrap()).unwrap();
    drop(index);
    drop(repo);
    drop(MailService::open(&root).unwrap());
    assert_eq!(fs::read_to_string(root.join("state.md")).unwrap(), original);
    let repo = Repository::open(&root).unwrap();
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(!repo.path().join("orchard-mail-transaction.json").exists());
}

#[test]
fn retires_marker_left_after_exact_commit() {
    let (_temp, root) = mailbox();
    let mut mail = MailService::open(&root).unwrap();
    register(&mut mail, "ra", "alice");
    drop(mail);
    let repo = Repository::open(&root).unwrap();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    let parent = commit.parent_id(0).unwrap();
    fs::write(
        repo.path().join("orchard-mail-transaction.json"),
        serde_json::to_vec_pretty(&json!({
            "head":parent.to_string(), "paths":["participants/alice.md","requests/ra.md"],
            "summary":"mail_register ra", "expected_tree":commit.tree_id().to_string()
        }))
        .unwrap(),
    )
    .unwrap();
    drop(commit);
    drop(repo);
    drop(MailService::open(&root).unwrap());
    assert!(!Repository::open(&root)
        .unwrap()
        .path()
        .join("orchard-mail-transaction.json")
        .exists());
}

#[test]
fn crash_marker_never_owns_an_unlisted_dirty_path() {
    let (_temp, root) = mailbox();
    drop(MailService::open(&root).unwrap());
    let repo = Repository::open(&root).unwrap();
    let head = repo.head().unwrap().target().unwrap();
    fs::write(root.join("state.md"), "partial").unwrap();
    fs::write(root.join("foreign.txt"), "keep me").unwrap();
    fs::write(
        repo.path().join("orchard-mail-transaction.json"),
        serde_json::to_vec_pretty(&json!({
            "head":head.to_string(), "paths":["state.md"], "summary":"owned", "expected_tree":null
        }))
        .unwrap(),
    )
    .unwrap();
    drop(repo);
    assert!(matches!(MailService::open(&root), Err(MailError::Dirty(_))));
    assert_eq!(
        fs::read_to_string(root.join("foreign.txt")).unwrap(),
        "keep me"
    );
}

#[test]
fn latest_history_returns_newest_bounded_window_in_ascending_order() {
    let (_temp, root) = mailbox();
    let mut mail = MailService::open(&root).unwrap();
    register(&mut mail, "ra", "alice");
    for sequence in 1..=205 {
        send(
            &mut mail,
            &format!("s{sequence}"),
            "alice",
            json!({"kind":"broadcast"}),
            &format!("message {sequence}"),
        );
    }
    let latest = mail
        .call("mail_history", json!({"latest":true,"limit":10}))
        .unwrap();
    let messages = latest["messages"].as_array().unwrap();
    assert_eq!(messages.first().unwrap()["sequence"], 196);
    assert_eq!(messages.last().unwrap()["sequence"], 205);
    let earliest = mail.call("mail_history", json!({"limit":10})).unwrap();
    assert_eq!(earliest["messages"][0]["sequence"], 1);
}

#[test]
fn observed_contact_is_transient_and_unknown_thread_is_rejected() {
    let (_temp, root) = mailbox();
    let mut mail = MailService::open(&root).unwrap();
    register(&mut mail, "ra", "alice");
    let participants = mail.call("mail_participants", json!({})).unwrap();
    assert!(participants["participants"][0]["last_contact_at"].is_string());
    assert!(matches!(
        mail.call(
            "mail_send",
            json!({
                "request_id":"bad-thread", "sender_id":"alice",
                "destination":{"kind":"broadcast"}, "body":"reply", "thread_id":"m_00000000000000000999"
            })
        ),
        Err(MailError::NotFound(_))
    ));
    drop(mail);
    let mut reopened = MailService::open(&root).unwrap();
    let participants = reopened.call("mail_participants", json!({})).unwrap();
    assert!(participants["participants"][0]
        .get("last_contact_at")
        .is_none());
}

#[test]
fn live_service_refuses_an_out_of_band_head_move() {
    let (_temp, root) = mailbox();
    let mut mail = MailService::open(&root).unwrap();
    commit_file(&root, "foreign.txt", "preserve", "outside writer");
    let result = mail.call(
        "mail_register",
        json!({"request_id":"ra","name":"Alice","participant_id":"alice"}),
    );
    assert!(matches!(result, Err(MailError::Dirty(message)) if message.contains("HEAD moved")));
    assert_eq!(
        fs::read_to_string(root.join("foreign.txt")).unwrap(),
        "preserve"
    );
}

#[test]
fn replay_rejects_modification_of_an_immutable_message() {
    let (_temp, root) = mailbox();
    {
        let mut mail = MailService::open(&root).unwrap();
        register(&mut mail, "ra", "alice");
        send(
            &mut mail,
            "send",
            "alice",
            json!({"kind":"broadcast"}),
            "hello",
        );
    }
    let message_path = "messages/m_00000000000000000001.md";
    let message = fs::read_to_string(root.join(message_path))
        .unwrap()
        .replace("\"body\": \"hello\"", "\"body\": \"forged\"");
    fs::write(root.join(message_path), message).unwrap();
    let forged_request = fs::read_to_string(root.join("requests/send.md"))
        .unwrap()
        .replace("\"request_id\": \"send\"", "\"request_id\": \"forged\"");
    fs::write(root.join("requests/forged.md"), forged_request).unwrap();
    let state = fs::read_to_string(root.join("state.md")).unwrap();
    fs::write(root.join("state.md"), format!("{state}\n")).unwrap();
    commit_paths(
        &root,
        &[message_path, "requests/forged.md", "state.md"],
        "mail_send forged",
        true,
    );
    assert!(matches!(
        MailService::open(&root),
        Err(MailError::Corrupt(_))
    ));
}
