use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use wait_timeout::ChildExt;

use crate::config::TaskStoreConfig;

pub(crate) const SUPPORTED_BR_VERSION: &str = "0.1.14";
pub(crate) const SUPPORTED_SCHEMA_VERSION: u32 = 1;
const LOCK_TIMEOUT_MS: &str = "1500";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const PINNED_SCHEMA_HASH: &str = "04f5625bedf2b5c78fbcb935730d250d25c4076151d9955f972ee45323968099";

#[derive(Clone)]
pub(crate) struct BeadsAdapter {
    br_path: PathBuf,
    unavailable: Option<String>,
}

#[derive(Debug)]
pub(crate) struct CommandFailure {
    pub message: String,
    pub unknown_outcome: bool,
}

pub(crate) struct CreateTask<'a> {
    pub workspace_id: &'a str,
    pub request_id: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub priority: Option<u8>,
    pub labels: &'a [String],
}

impl BeadsAdapter {
    pub(crate) fn open(br_path: PathBuf) -> Self {
        let mut version_command = Command::new(&br_path);
        version_command.arg("--version");
        let check = bounded_output(&mut version_command, false);
        let unavailable = match check {
            Err(error) => Some(format!(
                "cannot execute bundled br at {}: {}",
                br_path.display(),
                error.message
            )),
            Ok(output) if !output.status.success() => Some(format!(
                "bundled br version check failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let observed = stdout
                    .split_whitespace()
                    .find(|word| word.chars().next().is_some_and(|c| c.is_ascii_digit()))
                    .unwrap_or_default();
                (observed != SUPPORTED_BR_VERSION).then(|| format!(
                    "unsupported br version {observed:?}; Orchard requires exactly {SUPPORTED_BR_VERSION}"
                ))
            }
        };
        Self {
            br_path,
            unavailable,
        }
    }

    pub(crate) fn availability(&self) -> Result<(), String> {
        self.unavailable.clone().map_or(Ok(()), Err)
    }

    pub(crate) fn init_owned_store(
        &self,
        root: &Path,
        prefix: &str,
    ) -> Result<TaskStoreConfig, String> {
        self.availability()?;
        let beads_dir = root.join(".beads");
        std::fs::create_dir_all(&beads_dir)
            .map_err(|error| format!("cannot create owned task store: {error}"))?;
        let db_path = beads_dir.join("beads.db");
        if db_path.exists() {
            return Err(format!(
                "refusing to initialize over existing {}",
                db_path.display()
            ));
        }
        let store = TaskStoreConfig {
            id: String::new(),
            path: root.to_path_buf(),
            db_path: db_path.clone(),
            schema_version: SUPPORTED_SCHEMA_VERSION,
            source: Some("owned".to_owned()),
            repository_id: None,
        };
        let output = self
            .run_process(
                &store,
                &["init".to_owned(), "--prefix".to_owned(), prefix.to_owned()],
                true,
            )
            .map_err(|error| error.message)?;
        if !output.status.success() {
            return Err(format!(
                "could not initialize owned task store: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        inspect_schema(&db_path)?;
        let flush = self
            .run_process(
                &store,
                &["sync".to_owned(), "--flush-only".to_owned()],
                true,
            )
            .map_err(|error| error.message)?;
        if !flush.status.success() {
            return Err(format!(
                "owned task store initialized but initial JSONL flush failed: {}",
                String::from_utf8_lossy(&flush.stderr).trim()
            ));
        }
        Ok(store)
    }

    pub(crate) fn inspect_store(path: &Path) -> Result<TaskStoreConfig, String> {
        let (store_root, db_path) = resolve_store_paths(path)?;
        inspect_schema(&db_path)?;
        Ok(TaskStoreConfig {
            id: String::new(),
            path: store_root,
            db_path,
            schema_version: SUPPORTED_SCHEMA_VERSION,
            source: None,
            repository_id: None,
        })
    }

    pub(crate) fn list(
        &self,
        store: &TaskStoreConfig,
        status: Option<&str>,
    ) -> Result<Value, CommandFailure> {
        self.preflight(store)?;
        let mut args = vec!["list".to_owned(), "--limit".to_owned(), "0".to_owned()];
        if let Some(status) = status {
            args.push("--status".to_owned());
            args.push(status.to_owned());
        } else {
            args.push("--all".to_owned());
        }
        self.run(store, &args, false)
            .map(|tasks| qualify_list(&store.id, tasks))
    }

    pub(crate) fn show(
        &self,
        store: &TaskStoreConfig,
        task_id: &str,
    ) -> Result<Value, CommandFailure> {
        self.preflight(store)?;
        let value = self.run(store, &["show".to_owned(), task_id.to_owned()], false)?;
        let task = first_task(value).map_err(|message| CommandFailure {
            message,
            unknown_outcome: false,
        })?;
        Ok(qualify_task(&store.id, task))
    }

    pub(crate) fn dependencies(
        &self,
        store: &TaskStoreConfig,
        task_id: &str,
    ) -> Result<Value, CommandFailure> {
        self.preflight(store)?;
        let value = self.run(
            store,
            &[
                "dep".to_owned(),
                "list".to_owned(),
                task_id.to_owned(),
                "--direction".to_owned(),
                "both".to_owned(),
            ],
            false,
        )?;
        Ok(json!({
            "task_ref": {"store_id": store.id, "task_id": task_id},
            "dependencies": value
        }))
    }

    pub(crate) fn create(
        &self,
        store: &TaskStoreConfig,
        request: CreateTask<'_>,
    ) -> Result<Value, CommandFailure> {
        self.preflight(store)?;
        let external_ref =
            request_external_ref(request.workspace_id, &store.id, request.request_id);
        if let Some(task) = self.find_by_external_ref(store, &external_ref)? {
            return Ok(json!({
                "task": qualify_task(&store.id, task),
                "request_id": request.request_id,
                "reconciled": true,
                "application_status": "observed_existing_by_external_ref"
            }));
        }

        let mut args = vec![
            "create".to_owned(),
            "--title".to_owned(),
            request.title.to_owned(),
            "--external-ref".to_owned(),
            external_ref.clone(),
        ];
        if let Some(description) = request.description {
            args.push("--description".to_owned());
            args.push(description.to_owned());
        }
        if let Some(priority) = request.priority {
            args.push("--priority".to_owned());
            args.push(priority.to_string());
        }
        if !request.labels.is_empty() {
            args.push("--labels".to_owned());
            args.push(request.labels.join(","));
        }

        match self.run(store, &args, true) {
            Ok(value) => {
                let task = normalize_mutation_task(self, store, value)?;
                Ok(
                    json!({"task": task, "request_id": request.request_id, "reconciled": false, "application_status":"br_success"}),
                )
            }
            Err(error) => {
                if let Some(task) = self.find_by_external_ref(store, &external_ref)? {
                    let persistence_warning = self.flush_reconciled(store).err();
                    Ok(json!({
                        "task": qualify_task(&store.id, task),
                        "request_id": request.request_id,
                        "reconciled": true,
                        "application_status": "observed_after_unknown",
                        "command_warning": error.message,
                        "persistence_warning": persistence_warning
                    }))
                } else {
                    Err(CommandFailure {
                        message: format!(
                            "task create outcome is unknown after br failed; inspect external_ref {external_ref} before retry: {}",
                            error.message
                        ),
                        unknown_outcome: true,
                    })
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn update(
        &self,
        store: &TaskStoreConfig,
        task_id: &str,
        request_id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<u8>,
        add_labels: &[String],
        remove_labels: &[String],
    ) -> Result<Value, CommandFailure> {
        self.preflight(store)?;
        let mut args = vec!["update".to_owned(), task_id.to_owned()];
        if let Some(title) = title {
            args.push("--title".to_owned());
            args.push(title.to_owned());
        }
        if let Some(description) = description {
            args.push("--description".to_owned());
            args.push(description.to_owned());
        }
        if let Some(status) = status {
            args.push("--status".to_owned());
            args.push(status.to_owned());
        }
        if let Some(priority) = priority {
            args.push("--priority".to_owned());
            args.push(priority.to_string());
        }
        for label in add_labels {
            args.push("--add-label".to_owned());
            args.push(label.clone());
        }
        for label in remove_labels {
            args.push("--remove-label".to_owned());
            args.push(label.clone());
        }
        if args.len() == 2 {
            return Err(CommandFailure {
                message: "task_update requires at least one changed field".to_owned(),
                unknown_outcome: false,
            });
        }

        match self.run(store, &args, true) {
            Ok(value) => {
                let task = normalize_mutation_task(self, store, value)?;
                Ok(
                    json!({"task": task, "request_id": request_id, "reconciled": false, "application_status":"br_success"}),
                )
            }
            Err(error) => {
                let task = self.read_task_direct(store, task_id)?;
                if update_matches(
                    &task,
                    title,
                    description,
                    status,
                    priority,
                    add_labels,
                    remove_labels,
                ) {
                    let persistence_warning = self.flush_reconciled(store).err();
                    Ok(json!({
                        "task": qualify_task(&store.id, task),
                        "request_id": request_id,
                        "reconciled": true,
                        "application_status": "observed_after_unknown",
                        "command_warning": error.message,
                        "persistence_warning": persistence_warning
                    }))
                } else {
                    Err(CommandFailure {
                        message: format!(
                            "task update outcome is unknown; current task does not prove request {request_id} applied: {}",
                            error.message
                        ),
                        unknown_outcome: true,
                    })
                }
            }
        }
    }

    pub(crate) fn close(
        &self,
        store: &TaskStoreConfig,
        task_id: &str,
        request_id: &str,
        reason: Option<&str>,
    ) -> Result<Value, CommandFailure> {
        self.preflight(store)?;
        let mut args = vec!["close".to_owned(), task_id.to_owned()];
        if let Some(reason) = reason {
            args.push("--reason".to_owned());
            args.push(reason.to_owned());
        }
        match self.run(store, &args, true) {
            Ok(value) => {
                let task = normalize_mutation_task(self, store, value)?;
                Ok(
                    json!({"task": task, "request_id": request_id, "reconciled": false, "application_status":"br_success"}),
                )
            }
            Err(error) => {
                let task = self.read_task_direct(store, task_id)?;
                if task.get("status").and_then(Value::as_str) == Some("closed") {
                    let requested_reason_observed = reason.is_some_and(|requested| {
                        task.get("close_reason").and_then(Value::as_str) == Some(requested)
                    });
                    let persistence_warning = self.flush_reconciled(store).err();
                    Ok(json!({
                        "task": qualify_task(&store.id, task),
                        "request_id": request_id,
                        "reconciled": true,
                        "application_status": "observed_closed_after_unknown",
                        "requested_reason_observed": requested_reason_observed,
                        "command_warning": error.message,
                        "persistence_warning": persistence_warning
                    }))
                } else {
                    Err(CommandFailure {
                        message: format!(
                            "task close outcome is unknown; task is not observed closed for request {request_id}: {}",
                            error.message
                        ),
                        unknown_outcome: true,
                    })
                }
            }
        }
    }

    pub(crate) fn reconcile_create(
        &self,
        store: &TaskStoreConfig,
        workspace_id: &str,
        request_id: &str,
    ) -> Result<Option<Value>, CommandFailure> {
        self.preflight(store)?;
        let external_ref = request_external_ref(workspace_id, &store.id, request_id);
        self.find_by_external_ref(store, &external_ref).map(|task| {
            task.map(|task| {
                let persistence_warning = self.flush_reconciled(store).err();
                json!({
                    "task": qualify_task(&store.id, task),
                    "request_id": request_id,
                    "reconciled": true,
                    "application_status": "observed_after_pending_intent",
                    "persistence_warning": persistence_warning
                })
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reconcile_update(
        &self,
        store: &TaskStoreConfig,
        task_id: &str,
        request_id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<u8>,
        add_labels: &[String],
        remove_labels: &[String],
    ) -> Result<Option<Value>, CommandFailure> {
        self.preflight(store)?;
        let task = self.read_task_direct(store, task_id)?;
        Ok(update_matches(
            &task,
            title,
            description,
            status,
            priority,
            add_labels,
            remove_labels,
        )
        .then(|| {
            let persistence_warning = self.flush_reconciled(store).err();
            json!({
                "task": qualify_task(&store.id, task),
                "request_id": request_id,
                "reconciled": true,
                "application_status": "observed_after_pending_intent",
                "persistence_warning": persistence_warning
            })
        }))
    }

    pub(crate) fn reconcile_close(
        &self,
        store: &TaskStoreConfig,
        task_id: &str,
        request_id: &str,
        reason: Option<&str>,
    ) -> Result<Option<Value>, CommandFailure> {
        self.preflight(store)?;
        let task = self.read_task_direct(store, task_id)?;
        Ok((task.get("status").and_then(Value::as_str) == Some("closed")).then(|| {
            let persistence_warning = self.flush_reconciled(store).err();
            json!({
            "requested_reason_observed": reason.is_some_and(|requested| task.get("close_reason").and_then(Value::as_str) == Some(requested)),
            "task": qualify_task(&store.id, task),
            "request_id": request_id,
            "reconciled": true,
            "application_status": "observed_closed_after_pending_intent",
            "persistence_warning": persistence_warning
        })}))
    }

    fn preflight(&self, store: &TaskStoreConfig) -> Result<(), CommandFailure> {
        self.availability().map_err(normal_failure)?;
        inspect_schema(&store.db_path).map_err(|message| CommandFailure {
            message,
            unknown_outcome: false,
        })?;
        if ensure_jsonl_fresh(store).is_err() {
            let import = self.run_process(
                store,
                &["sync".to_owned(), "--import-only".to_owned()],
                false,
            )?;
            if !import.status.success() {
                return Err(normal_failure(format!(
                    "external Beads JSONL changed and refresh failed: {}",
                    String::from_utf8_lossy(&import.stderr).trim()
                )));
            }
            inspect_schema(&store.db_path).map_err(normal_failure)?;
            ensure_jsonl_fresh(store).map_err(normal_failure)?;
        }
        Ok(())
    }

    fn run(
        &self,
        store: &TaskStoreConfig,
        args: &[String],
        mutation: bool,
    ) -> Result<Value, CommandFailure> {
        if mutation {
            ensure_jsonl_fresh(store).map_err(normal_failure)?;
        }
        let output = self.run_process(store, args, mutation)?;
        if !output.status.success() {
            return Err(CommandFailure {
                message: format!(
                    "br exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
                unknown_outcome: mutation,
            });
        }
        let value = serde_json::from_slice(&output.stdout).map_err(|error| CommandFailure {
            message: format!("br returned invalid JSON: {error}"),
            unknown_outcome: mutation,
        })?;
        if mutation {
            ensure_jsonl_fresh(store).map_err(|message| CommandFailure {
                message: format!(
                    "task command returned, but JSONL changed before Orchard could flush; refusing to overwrite it: {message}"
                ),
                unknown_outcome: true,
            })?;
            let flush =
                self.run_process(store, &["sync".to_owned(), "--flush-only".to_owned()], true)?;
            if !flush.status.success() {
                return Err(CommandFailure {
                    message: format!(
                        "task changed in SQLite but JSONL flush failed: {}",
                        String::from_utf8_lossy(&flush.stderr).trim()
                    ),
                    unknown_outcome: true,
                });
            }
        }
        Ok(value)
    }

    fn run_process(
        &self,
        store: &TaskStoreConfig,
        args: &[String],
        mutation: bool,
    ) -> Result<std::process::Output, CommandFailure> {
        let mut command = Command::new(&self.br_path);
        command
            .current_dir(&store.path)
            .arg("--db")
            .arg(&store.db_path)
            .arg("--no-auto-import")
            .arg("--no-auto-flush")
            .arg("--lock-timeout")
            .arg(LOCK_TIMEOUT_MS)
            .arg("--allow-stale")
            .arg("--json")
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        bounded_output(&mut command, mutation)
    }

    fn flush_reconciled(&self, store: &TaskStoreConfig) -> Result<(), String> {
        ensure_jsonl_fresh(store).map_err(|message| {
            format!("refusing reconciliation flush because external JSONL changed: {message}")
        })?;
        let flush = self
            .run_process(store, &["sync".to_owned(), "--flush-only".to_owned()], true)
            .map_err(|error| error.message)?;
        if flush.status.success() {
            Ok(())
        } else {
            Err(format!(
                "JSONL flush after reconciliation failed: {}",
                String::from_utf8_lossy(&flush.stderr).trim()
            ))
        }
    }

    fn find_by_external_ref(
        &self,
        store: &TaskStoreConfig,
        external_ref: &str,
    ) -> Result<Option<Value>, CommandFailure> {
        let connection = open_readonly(&store.db_path).map_err(normal_failure)?;
        let id: Option<String> = connection
            .query_row(
                "SELECT id FROM issues WHERE external_ref = ?1 AND deleted_at IS NULL LIMIT 1",
                [external_ref],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| normal_failure(format!("cannot reconcile task create: {error}")))?;
        id.map(|id| self.read_task_direct(store, &id)).transpose()
    }

    fn read_task_direct(
        &self,
        store: &TaskStoreConfig,
        task_id: &str,
    ) -> Result<Value, CommandFailure> {
        let value = self.run(store, &["show".to_owned(), task_id.to_owned()], false)?;
        first_task(value).map_err(normal_failure)
    }
}

fn bounded_output(
    command: &mut Command,
    mutation: bool,
) -> Result<std::process::Output, CommandFailure> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| CommandFailure {
        message: format!("failed to launch bundled br: {error}"),
        unknown_outcome: mutation,
    })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_reader = thread::spawn(move || read_capped(stdout));
    let stderr_reader = thread::spawn(move || read_capped(stderr));
    let status = match child.wait_timeout(COMMAND_TIMEOUT) {
        Ok(Some(status)) => status,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(CommandFailure {
                message: format!(
                    "br exceeded the {} second command deadline",
                    COMMAND_TIMEOUT.as_secs()
                ),
                unknown_outcome: mutation,
            });
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(CommandFailure {
                message: format!("could not wait for br: {error}"),
                unknown_outcome: mutation,
            });
        }
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| normal_failure("br stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| normal_failure("br stderr reader panicked"))??;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn resolve_store_paths(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("task store path {} is unavailable: {error}", path.display()))?;
    if canonical.is_file() {
        if canonical.file_name().and_then(|name| name.to_str()) != Some("beads.db") {
            return Err("task store database must be named beads.db".to_owned());
        }
        let beads_dir = canonical
            .parent()
            .ok_or_else(|| "beads.db has no parent directory".to_owned())?;
        let root = if beads_dir.file_name().and_then(|name| name.to_str()) == Some(".beads") {
            beads_dir.parent().unwrap_or(beads_dir)
        } else {
            beads_dir
        };
        return Ok((root.to_path_buf(), canonical));
    }

    let direct_db = canonical.join("beads.db");
    let nested_db = canonical.join(".beads").join("beads.db");
    let db_path = if direct_db.is_file() {
        direct_db
    } else if nested_db.is_file() {
        nested_db
    } else {
        return Err(format!(
            "{} is not a classic Beads store (expected .beads/beads.db)",
            canonical.display()
        ));
    };
    Ok((
        canonical,
        db_path.canonicalize().map_err(|error| error.to_string())?,
    ))
}

pub(crate) fn inspect_schema(db_path: &Path) -> Result<(), String> {
    let connection = open_readonly(db_path)?;
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|error| format!("cannot read Beads schema version: {error}"))?;
    if version != SUPPORTED_SCHEMA_VERSION {
        return Err(format!(
            "unsupported Beads schema version {version}; only classic br {SUPPORTED_BR_VERSION} schema {SUPPORTED_SCHEMA_VERSION} is writable"
        ));
    }

    let mut schema_statement = connection
        .prepare(
            "SELECT name, sql FROM sqlite_master \
             WHERE type IN ('table','index') AND sql IS NOT NULL ORDER BY type, name",
        )
        .map_err(|error| format!("cannot inspect Beads schema objects: {error}"))?;
    let schema_objects: Vec<(String, String)> = schema_statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|error| format!("cannot inspect Beads schema objects: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("cannot inspect Beads schema objects: {error}"))?;
    let encoded = serde_json::to_vec(&schema_objects)
        .map_err(|error| format!("cannot fingerprint Beads schema: {error}"))?;
    let observed_schema_hash = format!("{:x}", Sha256::digest(encoded));
    if observed_schema_hash != PINNED_SCHEMA_HASH {
        return Err(format!(
            "unsupported Beads schema fingerprint {observed_schema_hash}; expected the reviewed br {SUPPORTED_BR_VERSION} schema"
        ));
    }

    require_columns(
        &connection,
        "issues",
        &[
            "id",
            "content_hash",
            "title",
            "description",
            "design",
            "acceptance_criteria",
            "notes",
            "status",
            "priority",
            "issue_type",
            "assignee",
            "owner",
            "estimated_minutes",
            "created_at",
            "created_by",
            "updated_at",
            "closed_at",
            "close_reason",
            "closed_by_session",
            "due_at",
            "defer_until",
            "external_ref",
            "source_system",
            "source_repo",
            "deleted_at",
            "deleted_by",
            "delete_reason",
            "original_type",
            "compaction_level",
            "compacted_at",
            "compacted_at_commit",
            "original_size",
            "sender",
            "ephemeral",
            "pinned",
            "is_template",
        ],
    )?;
    require_columns(
        &connection,
        "dependencies",
        &[
            "issue_id",
            "depends_on_id",
            "type",
            "created_at",
            "created_by",
            "metadata",
            "thread_id",
        ],
    )?;
    require_columns(&connection, "labels", &["issue_id", "label"])?;
    require_columns(
        &connection,
        "comments",
        &["id", "issue_id", "author", "text", "created_at"],
    )?;
    require_columns(
        &connection,
        "events",
        &[
            "id",
            "issue_id",
            "event_type",
            "actor",
            "old_value",
            "new_value",
            "comment",
            "created_at",
        ],
    )?;
    require_columns(&connection, "config", &["key", "value"])?;
    require_columns(&connection, "metadata", &["key", "value"])?;
    require_columns(&connection, "dirty_issues", &["issue_id", "marked_at"])?;
    require_columns(
        &connection,
        "export_hashes",
        &["issue_id", "content_hash", "exported_at"],
    )?;
    require_columns(
        &connection,
        "blocked_issues_cache",
        &["issue_id", "blocked_by", "blocked_at"],
    )?;
    require_columns(&connection, "child_counters", &["parent_id", "last_child"])?;
    Ok(())
}

fn ensure_jsonl_fresh(store: &TaskStoreConfig) -> Result<(), String> {
    let jsonl_path = store
        .db_path
        .parent()
        .ok_or_else(|| "task database has no parent".to_owned())?
        .join("issues.jsonl");
    let connection = open_readonly(&store.db_path)?;
    let recorded: Option<String> = connection
        .query_row(
            "SELECT value FROM metadata WHERE key = 'jsonl_content_hash'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| format!("cannot read Beads JSONL watermark: {error}"))?;
    match (recorded, jsonl_path.exists()) {
        (None, false) => Ok(()),
        (Some(expected), true) => {
            let mut file = std::fs::File::open(&jsonl_path)
                .map_err(|error| format!("cannot inspect {}: {error}", jsonl_path.display()))?;
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = file
                    .read(&mut buffer)
                    .map_err(|error| format!("cannot inspect {}: {error}", jsonl_path.display()))?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            let observed = format!("{:x}", hasher.finalize());
            if observed == expected {
                Ok(())
            } else {
                Err("issues.jsonl changed outside Orchard; refusing mutation until the store is refreshed with its pinned br".to_owned())
            }
        }
        _ => Err(
            "Beads JSONL watermark and issues.jsonl presence disagree; refusing mutation"
                .to_owned(),
        ),
    }
}

fn read_capped(mut reader: impl Read) -> Result<Vec<u8>, CommandFailure> {
    let mut result = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| normal_failure(format!("cannot read br output: {error}")))?;
        if count == 0 {
            return Ok(result);
        }
        if result.len() + count > OUTPUT_LIMIT {
            return Err(normal_failure(format!(
                "br output exceeded {OUTPUT_LIMIT} bytes"
            )));
        }
        result.extend_from_slice(&buffer[..count]);
    }
}

fn open_readonly(db_path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| format!("cannot open task store read-only: {error}"))
}

fn require_columns(connection: &Connection, table: &str, required: &[&str]) -> Result<(), String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("cannot inspect {table}: {error}"))?;
    let columns: BTreeSet<String> = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("cannot inspect {table}: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("cannot inspect {table}: {error}"))?;
    for column in required {
        if !columns.contains(*column) {
            return Err(format!(
                "unsupported Beads schema: table {table} is missing column {column}"
            ));
        }
    }
    Ok(())
}

fn request_external_ref(workspace_id: &str, store_id: &str, request_id: &str) -> String {
    format!("orchard:{workspace_id}:{store_id}:{request_id}")
}

fn first_task(value: Value) -> Result<Value, String> {
    match value {
        Value::Array(mut values) if values.len() == 1 => Ok(values.remove(0)),
        Value::Object(_) => Ok(value),
        Value::Array(values) if values.is_empty() => Err("task was not found".to_owned()),
        _ => Err("br returned an unexpected task shape".to_owned()),
    }
}

fn normalize_mutation_task(
    adapter: &BeadsAdapter,
    store: &TaskStoreConfig,
    value: Value,
) -> Result<Value, CommandFailure> {
    let candidate = match &value {
        Value::Object(object) => object
            .get("id")
            .or_else(|| object.get("task_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        Value::Array(values) => values
            .first()
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    };
    if let Some(task_id) = candidate {
        return adapter
            .read_task_direct(store, &task_id)
            .map(|task| qualify_task(&store.id, task));
    }
    let task = first_task(value).map_err(normal_failure)?;
    Ok(qualify_task(&store.id, task))
}

fn qualify_list(store_id: &str, value: Value) -> Value {
    let tasks = match value {
        Value::Array(values) => values
            .into_iter()
            .map(|task| qualify_task(store_id, task))
            .collect(),
        other => vec![qualify_task(store_id, other)],
    };
    json!({"store_id": store_id, "tasks": tasks})
}

fn qualify_task(store_id: &str, mut task: Value) -> Value {
    if let Value::Object(ref mut object) = task {
        let task_id = object.get("id").cloned().unwrap_or(Value::Null);
        object.insert(
            "task_ref".to_owned(),
            json!({"store_id": store_id, "task_id": task_id}),
        );
    }
    task
}

fn update_matches(
    task: &Value,
    title: Option<&str>,
    description: Option<&str>,
    status: Option<&str>,
    priority: Option<u8>,
    add_labels: &[String],
    remove_labels: &[String],
) -> bool {
    if title.is_some_and(|expected| task.get("title").and_then(Value::as_str) != Some(expected)) {
        return false;
    }
    if description
        .is_some_and(|expected| task.get("description").and_then(Value::as_str) != Some(expected))
    {
        return false;
    }
    if status.is_some_and(|expected| task.get("status").and_then(Value::as_str) != Some(expected)) {
        return false;
    }
    if priority.is_some_and(|expected| {
        task.get("priority").and_then(Value::as_u64) != Some(expected as u64)
    }) {
        return false;
    }
    let labels: BTreeSet<&str> = task
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    add_labels
        .iter()
        .all(|label| labels.contains(label.as_str()))
        && remove_labels
            .iter()
            .all(|label| !labels.contains(label.as_str()))
}

fn normal_failure(message: impl Into<String>) -> CommandFailure {
    CommandFailure {
        message: message.into(),
        unknown_outcome: false,
    }
}
