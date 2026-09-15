use orchard_mail_core::MailService;
use std::{fs, process::Command};
use tempfile::TempDir;

#[test]
fn standalone_call_works_and_refuses_an_active_writer() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("mail");
    let output = Command::new(env!("CARGO_BIN_EXE_orchard-mail"))
        .args([
            "call",
            "--root",
            root.to_str().unwrap(),
            "mail_register",
            r#"{"request_id":"r1","name":"Alice","participant_id":"alice"}"#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["participant"]["id"], "alice");

    let _held = MailService::open(&root).unwrap();
    let blocked = Command::new(env!("CARGO_BIN_EXE_orchard-mail"))
        .args([
            "call",
            "--root",
            root.to_str().unwrap(),
            "mail_channels",
            "{}",
        ])
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("already open"));
}

#[test]
fn server_rejects_token_file_inside_mailbox() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("mail");
    drop(MailService::open(&root).unwrap());
    let token = root.join("token.txt");
    fs::write(&token, "secret").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orchard-mail"))
        .args([
            "serve",
            "--root",
            root.to_str().unwrap(),
            "--token-file",
            token.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("outside"));
}
