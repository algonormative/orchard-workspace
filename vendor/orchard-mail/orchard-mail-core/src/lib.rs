//! Git-backed local mailboxes for cooperating software agents.
//!
//! `MailService` is deliberately synchronous: one process owns an exclusive
//! writer lock and every successful durable mutation becomes one Git commit.
//! Cooperative participant names are labels only. They are not authenticated
//! provider identities, and a `direct` destination is not a private-message
//! confidentiality boundary.

use chrono::{DateTime, Utc};
use fs2::FileExt;
use git2::{build::CheckoutBuilder, Repository, Signature, StatusOptions};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;
use uuid::Uuid;

/// Result type used by Orchard Mail core.
pub type Result<T> = std::result::Result<T, MailError>;

/// Errors are intentionally descriptive because a dirty or corrupt repository
/// must be repaired by a human rather than overwritten automatically.
#[derive(Debug, Error)]
pub enum MailError {
    #[error("mail repository is already open by another writer: {0}")]
    Locked(PathBuf),
    #[error("mail repository has unexpected uncommitted changes: {0}")]
    Dirty(String),
    #[error("mail repository is corrupt or unreadable: {0}")]
    Corrupt(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("request_id conflict for {0}")]
    RequestConflict(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Git error: {0}")]
    Git(#[from] git2::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// A stable cooperative identity. Session and last-contact fields are advisory
/// process-local observations, never committed to Git or liveness claims.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub registered_at: DateTime<Utc>,
    pub registered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_instance_id: Option<String>,
    /// Last contact observed by this process. Presence is advisory and is not
    /// restored from Git after a restart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_contact_at: Option<DateTime<Utc>>,
}

/// A public delivery group. `general` is created with every repository.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Channel {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Message destination. Recipient membership is snapshotted when a message is
/// sent, so later registration or leave operations never rewrite old delivery.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Destination {
    Channel { id: String },
    Direct { id: String },
    Broadcast,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DestinationKind {
    Channel,
    Direct,
    Broadcast,
}

/// Canonical immutable message record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub sequence: u64,
    /// Durable idempotency identity of the send operation that created this
    /// immutable message.
    pub request_id: String,
    pub sent_at: DateTime<Utc>,
    pub sender_id: String,
    pub destination: Destination,
    pub body: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub refs: Vec<Value>,
    /// Stable recipient identity snapshot used by inbox retrieval.
    pub recipient_ids: Vec<String>,
}

fn default_kind() -> String {
    "message".to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredParticipant {
    id: String,
    name: String,
    registered_at: DateTime<Utc>,
    registered: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AckRecord {
    participant_id: String,
    message_ids: Vec<String>,
    acknowledged_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct RequestRecord {
    request_id: String,
    operation: String,
    fingerprint: String,
    result: Value,
    committed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StateRecord {
    next_sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CrashTransaction {
    head: String,
    paths: Vec<String>,
    summary: String,
    expected_tree: Option<String>,
}

#[derive(Clone)]
struct SessionPresence {
    id: Option<String>,
    last_contact_at: DateTime<Utc>,
}

#[derive(Default)]
struct MailIndex {
    participants: BTreeMap<String, StoredParticipant>,
    channels: BTreeMap<String, Channel>,
    messages: Vec<Message>,
    acknowledgements: BTreeMap<String, BTreeSet<String>>,
    requests: HashMap<String, RequestRecord>,
    next_sequence: u64,
}

/// Single-writer mailbox service.
///
/// Opening takes an OS advisory lock stored inside `.git`, validates repository
/// cleanliness, recovers only a transaction carrying Orchard Mail's own crash
/// marker, and rebuilds all indexes from canonical Markdown records.
pub struct MailService {
    root: PathBuf,
    repo: Repository,
    _lock: File,
    index: MailIndex,
    sessions: HashMap<String, SessionPresence>,
    poisoned: Option<String>,
    expected_head: Option<git2::Oid>,
    #[cfg(test)]
    fault_after_commit: bool,
}

impl MailService {
    /// Opens or initializes a mailbox repository and acquires its exclusive
    /// writer lock for the lifetime of the returned service.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        let (repo, initialized_here) = match Repository::open(&root) {
            Ok(repo) => (repo, false),
            Err(_) => {
                let nonempty = fs::read_dir(&root)?.next().transpose()?.is_some();
                if nonempty {
                    return Err(MailError::Corrupt(format!(
                        "{} is non-empty and is not a Git repository",
                        root.display()
                    )));
                }
                (Repository::init(&root)?, true)
            }
        };
        let lock_path = repo.path().join("orchard-mail.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        lock.try_lock_exclusive()
            .map_err(|_| MailError::Locked(root.clone()))?;

        let mut service = Self {
            root,
            repo,
            _lock: lock,
            index: MailIndex::default(),
            sessions: HashMap::new(),
            poisoned: None,
            expected_head: None,
            #[cfg(test)]
            fault_after_commit: false,
        };
        if initialized_here {
            service.initialize_repository()?;
        } else {
            service
                .repo
                .head()
                .map_err(|e| MailError::Corrupt(format!("repository has no valid HEAD: {e}")))?;
            service.recover_known_transaction()?;
            service.ensure_clean()?;
        }
        service.index = service.rebuild_index()?;
        service.expected_head = service.repo.head()?.target();
        Ok(service)
    }

    /// Calls one of the stable `mail_*` operations with a JSON object.
    /// Durable mutations require `request_id`; reads and `mail_resume` do not.
    pub fn call(&mut self, operation: &str, args: Value) -> Result<Value> {
        if let Some(reason) = &self.poisoned {
            return Err(MailError::Corrupt(format!(
                "service is poisoned; reopen required: {reason}"
            )));
        }
        if !args.is_object() {
            return Err(MailError::Invalid("arguments must be a JSON object".into()));
        }
        self.ensure_clean()?;
        match operation {
            "mail_register" => self.register(args),
            "mail_resume" => self.resume(args),
            "mail_leave" => self.leave(args),
            "mail_participants" => self.participants(args),
            "mail_channel_create" => self.channel_create(args),
            "mail_channels" => self.channels(args),
            "mail_send" => self.send(args),
            "mail_inbox" => self.inbox(args),
            "mail_history" => self.history(args),
            "mail_acknowledge" => self.acknowledge(args),
            "mail_search" => self.search(args),
            other => Err(MailError::Invalid(format!("unknown operation {other}"))),
        }
    }

    /// Filesystem root containing the mailbox Git repository.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn initialize_repository(&mut self) -> Result<()> {
        let now = Utc::now();
        let general = Channel {
            id: "general".into(),
            name: "general".into(),
            description: Some("Default public channel".into()),
            created_at: now,
        };
        let writes = vec![
            (
                "channels/general.md".into(),
                markdown(&general, "Default public channel")?,
            ),
            (
                "state.md".into(),
                markdown(&StateRecord { next_sequence: 1 }, "Orchard Mail state")?,
            ),
        ];
        self.apply_transaction("Initialize Orchard Mail", writes)
    }

    fn register(&mut self, args: Value) -> Result<Value> {
        if let Some(result) = self.retry_result("mail_register", &args)? {
            return Ok(result);
        }
        #[derive(Deserialize)]
        struct Args {
            request_id: String,
            name: String,
            participant_id: Option<String>,
        }
        let a: Args = parse(args.clone())?;
        validate_request_id(&a.request_id)?;
        if a.name.trim().is_empty() {
            return Err(MailError::Invalid("name must not be empty".into()));
        }
        let id = a
            .participant_id
            .unwrap_or_else(|| format!("p_{}", Uuid::new_v4().simple()));
        validate_id("participant_id", &id)?;
        if self
            .index
            .participants
            .get(&id)
            .is_some_and(|p| p.registered)
        {
            return Err(MailError::Invalid(format!(
                "participant {id} is already registered"
            )));
        }
        let stored = StoredParticipant {
            id: id.clone(),
            name: a.name,
            registered_at: Utc::now(),
            registered: true,
        };
        let participant = public_participant(&stored, None);
        let result = json!({"participant": participant});
        let writes = vec![(
            format!("participants/{id}.md"),
            markdown(&stored, &format!("Participant: {}", stored.name))?,
        )];
        self.commit_request(
            "mail_register",
            &args,
            &a.request_id,
            result.clone(),
            writes,
        )?;
        self.index.participants.insert(id.clone(), stored);
        self.touch(&id);
        Ok(result)
    }

    fn resume(&mut self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            participant_id: String,
        }
        let a: Args = parse(args)?;
        let stored = self.active_participant(&a.participant_id)?.clone();
        let presence = SessionPresence {
            id: Some(format!("s_{}", Uuid::new_v4().simple())),
            last_contact_at: Utc::now(),
        };
        self.sessions.insert(a.participant_id, presence.clone());
        Ok(json!({"participant": public_participant(&stored, Some(presence))}))
    }

    fn leave(&mut self, args: Value) -> Result<Value> {
        if let Some(result) = self.retry_result("mail_leave", &args)? {
            return Ok(result);
        }
        #[derive(Deserialize)]
        struct Args {
            request_id: String,
            participant_id: String,
        }
        let a: Args = parse(args.clone())?;
        validate_request_id(&a.request_id)?;
        let mut stored = self.active_participant(&a.participant_id)?.clone();
        stored.registered = false;
        let result = json!({"participant_id": a.participant_id, "registered": false});
        let writes = vec![(
            format!("participants/{}.md", stored.id),
            markdown(&stored, &format!("Participant: {}", stored.name))?,
        )];
        self.commit_request("mail_leave", &args, &a.request_id, result.clone(), writes)?;
        self.index.participants.insert(stored.id.clone(), stored);
        self.sessions.remove(&a.participant_id);
        Ok(result)
    }

    fn participants(&self, _args: Value) -> Result<Value> {
        let items: Vec<_> = self
            .index
            .participants
            .values()
            .map(|p| public_participant(p, self.sessions.get(&p.id).cloned()))
            .collect();
        Ok(json!({"participants": items}))
    }

    fn channel_create(&mut self, args: Value) -> Result<Value> {
        if let Some(result) = self.retry_result("mail_channel_create", &args)? {
            return Ok(result);
        }
        #[derive(Deserialize)]
        struct Args {
            request_id: String,
            name: String,
            description: Option<String>,
            channel_id: Option<String>,
        }
        let a: Args = parse(args.clone())?;
        validate_request_id(&a.request_id)?;
        let id = a.channel_id.unwrap_or_else(|| slug(&a.name));
        validate_id("channel_id", &id)?;
        if self.index.channels.contains_key(&id) {
            return Err(MailError::Invalid(format!("channel {id} already exists")));
        }
        if a.name.trim().is_empty() {
            return Err(MailError::Invalid("name must not be empty".into()));
        }
        let channel = Channel {
            id: id.clone(),
            name: a.name,
            description: a.description,
            created_at: Utc::now(),
        };
        let result = json!({"channel": channel});
        let writes = vec![(
            format!("channels/{id}.md"),
            markdown(
                &channel,
                channel.description.as_deref().unwrap_or("Public channel"),
            )?,
        )];
        self.commit_request(
            "mail_channel_create",
            &args,
            &a.request_id,
            result.clone(),
            writes,
        )?;
        self.index.channels.insert(id, channel);
        Ok(result)
    }

    fn channels(&self, _args: Value) -> Result<Value> {
        Ok(json!({"channels": self.index.channels.values().collect::<Vec<_>>() }))
    }

    fn send(&mut self, args: Value) -> Result<Value> {
        if let Some(result) = self.retry_result("mail_send", &args)? {
            return Ok(result);
        }
        #[derive(Deserialize)]
        struct Args {
            request_id: String,
            sender_id: String,
            destination: Destination,
            body: String,
            #[serde(default = "default_kind")]
            kind: String,
            thread_id: Option<String>,
            #[serde(default)]
            refs: Vec<Value>,
        }
        let a: Args = parse(args.clone())?;
        validate_request_id(&a.request_id)?;
        self.active_participant(&a.sender_id)?;
        if a.body.trim().is_empty() {
            return Err(MailError::Invalid("body must not be empty".into()));
        }
        if let Some(thread_id) = &a.thread_id {
            if !self
                .index
                .messages
                .iter()
                .any(|candidate| &candidate.id == thread_id)
            {
                return Err(MailError::NotFound(format!("thread message {thread_id}")));
            }
        }
        let recipients: Vec<String> = match &a.destination {
            Destination::Channel { id } => {
                if !self.index.channels.contains_key(id) {
                    return Err(MailError::NotFound(format!("channel {id}")));
                }
                self.active_ids()
            }
            Destination::Direct { id } => {
                self.active_participant(id)?;
                vec![id.clone()]
            }
            Destination::Broadcast => self.active_ids(),
        };
        let sequence = self.index.next_sequence;
        let message = Message {
            id: format!("m_{sequence:020}"),
            sequence,
            request_id: a.request_id.clone(),
            sent_at: Utc::now(),
            sender_id: a.sender_id.clone(),
            destination: a.destination,
            body: a.body,
            kind: a.kind,
            thread_id: a.thread_id,
            refs: a.refs,
            recipient_ids: recipients,
        };
        let result = json!({"message": message});
        let writes = vec![
            (
                format!("messages/{}.md", message.id),
                markdown(&message, &message.body)?,
            ),
            (
                "state.md".into(),
                markdown(
                    &StateRecord {
                        next_sequence: sequence + 1,
                    },
                    "Orchard Mail state",
                )?,
            ),
        ];
        self.commit_request("mail_send", &args, &a.request_id, result.clone(), writes)?;
        self.index.messages.push(message);
        self.index.next_sequence = sequence + 1;
        self.touch(&a.sender_id);
        Ok(result)
    }

    fn inbox(&mut self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            participant_id: String,
            after: Option<u64>,
            limit: Option<usize>,
        }
        let a: Args = parse(args)?;
        self.index
            .participants
            .get(&a.participant_id)
            .ok_or_else(|| MailError::NotFound(format!("participant {}", a.participant_id)))?;
        let limit = bounded_limit(a.limit)?;
        let acked = self.index.acknowledgements.get(&a.participant_id);
        let messages: Vec<Value> = self.index.messages.iter()
            .filter(|m| m.sequence > a.after.unwrap_or(0) && m.recipient_ids.contains(&a.participant_id))
            .take(limit)
            .map(|m| json!({"message": m, "acknowledged": acked.is_some_and(|set| set.contains(&m.id))}))
            .collect();
        self.touch(&a.participant_id);
        Ok(json!({"messages": messages}))
    }

    fn history(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            channel_id: Option<String>,
            sender_id: Option<String>,
            thread_id: Option<String>,
            destination_kind: Option<DestinationKind>,
            after: Option<u64>,
            limit: Option<usize>,
            #[serde(default)]
            latest: bool,
        }
        let a: Args = parse(args)?;
        let limit = bounded_limit(a.limit)?;
        let matching: Vec<&Message> = self.index.messages.iter().filter(|m| {
            m.sequence > a.after.unwrap_or(0)
                && a.sender_id.as_ref().is_none_or(|id| &m.sender_id == id)
                && a.thread_id.as_ref().is_none_or(|id| m.thread_id.as_ref() == Some(id))
                && a.destination_kind.as_ref().is_none_or(|kind| {
                    matches!((kind, &m.destination),
                        (DestinationKind::Channel, Destination::Channel { .. })
                        | (DestinationKind::Direct, Destination::Direct { .. })
                        | (DestinationKind::Broadcast, Destination::Broadcast))
                })
                && a.channel_id.as_ref().is_none_or(|id| matches!(&m.destination, Destination::Channel { id: actual } if actual == id))
        }).collect();
        let messages = if a.latest && matching.len() > limit {
            matching[matching.len() - limit..].to_vec()
        } else {
            matching.into_iter().take(limit).collect()
        };
        Ok(json!({"messages": messages}))
    }

    fn acknowledge(&mut self, args: Value) -> Result<Value> {
        if let Some(result) = self.retry_result("mail_acknowledge", &args)? {
            return Ok(result);
        }
        #[derive(Deserialize)]
        struct Args {
            request_id: String,
            participant_id: String,
            message_ids: Vec<String>,
        }
        let a: Args = parse(args.clone())?;
        validate_request_id(&a.request_id)?;
        self.index
            .participants
            .get(&a.participant_id)
            .ok_or_else(|| MailError::NotFound(format!("participant {}", a.participant_id)))?;
        if a.message_ids.is_empty() {
            return Err(MailError::Invalid("message_ids must not be empty".into()));
        }
        let delivered: BTreeSet<_> = self
            .index
            .messages
            .iter()
            .filter(|m| m.recipient_ids.contains(&a.participant_id))
            .map(|m| m.id.as_str())
            .collect();
        for id in &a.message_ids {
            if !delivered.contains(id.as_str()) {
                return Err(MailError::Invalid(format!(
                    "message {id} was not delivered to {}",
                    a.participant_id
                )));
            }
        }
        let record = AckRecord {
            participant_id: a.participant_id.clone(),
            message_ids: a.message_ids.clone(),
            acknowledged_at: Utc::now(),
        };
        let result = json!({"participant_id": a.participant_id, "message_ids": a.message_ids});
        let writes = vec![(
            format!("acknowledgements/{}.md", a.request_id),
            markdown(&record, "Explicit delivery acknowledgement")?,
        )];
        self.commit_request(
            "mail_acknowledge",
            &args,
            &a.request_id,
            result.clone(),
            writes,
        )?;
        self.index
            .acknowledgements
            .entry(record.participant_id)
            .or_default()
            .extend(record.message_ids);
        self.touch(&a.participant_id);
        Ok(result)
    }

    fn search(&self, args: Value) -> Result<Value> {
        #[derive(Deserialize)]
        struct Args {
            query: String,
            after: Option<u64>,
            limit: Option<usize>,
        }
        let a: Args = parse(args)?;
        if a.query.trim().is_empty() {
            return Err(MailError::Invalid("query must not be empty".into()));
        }
        let needle = a.query.to_lowercase();
        let limit = bounded_limit(a.limit)?;
        let messages: Vec<&Message> = self
            .index
            .messages
            .iter()
            .filter(|m| {
                m.sequence > a.after.unwrap_or(0)
                    && (m.body.to_lowercase().contains(&needle)
                        || m.kind.to_lowercase().contains(&needle)
                        || m.sender_id.to_lowercase().contains(&needle)
                        || m.thread_id
                            .as_ref()
                            .is_some_and(|v| v.to_lowercase().contains(&needle)))
            })
            .take(limit)
            .collect();
        Ok(json!({"messages": messages}))
    }

    fn active_participant(&self, id: &str) -> Result<&StoredParticipant> {
        self.index
            .participants
            .get(id)
            .filter(|p| p.registered)
            .ok_or_else(|| MailError::NotFound(format!("registered participant {id}")))
    }

    fn active_ids(&self) -> Vec<String> {
        self.index
            .participants
            .values()
            .filter(|p| p.registered)
            .map(|p| p.id.clone())
            .collect()
    }

    fn touch(&mut self, participant_id: &str) {
        let now = Utc::now();
        self.sessions
            .entry(participant_id.to_owned())
            .and_modify(|presence| presence.last_contact_at = now)
            .or_insert(SessionPresence {
                id: None,
                last_contact_at: now,
            });
    }

    fn retry_result(&self, operation: &str, args: &Value) -> Result<Option<Value>> {
        let request_id = args
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| MailError::Invalid("request_id is required for this mutation".into()))?;
        validate_request_id(request_id)?;
        if let Some(old) = self.index.requests.get(request_id) {
            let fingerprint = fingerprint(operation, args)?;
            if old.operation == operation && old.fingerprint == fingerprint {
                return Ok(Some(old.result.clone()));
            }
            return Err(MailError::RequestConflict(request_id.into()));
        }
        Ok(None)
    }

    fn commit_request(
        &mut self,
        operation: &str,
        args: &Value,
        request_id: &str,
        result: Value,
        mut writes: Vec<(String, String)>,
    ) -> Result<()> {
        let record = RequestRecord {
            request_id: request_id.into(),
            operation: operation.into(),
            fingerprint: fingerprint(operation, args)?,
            result,
            committed_at: Utc::now(),
        };
        writes.push((
            format!("requests/{request_id}.md"),
            markdown(&record, &format!("Idempotency record for `{operation}`"))?,
        ));
        self.apply_transaction(&format!("{operation} {request_id}"), writes)?;
        self.index.requests.insert(request_id.into(), record);
        Ok(())
    }

    fn apply_transaction(&mut self, summary: &str, writes: Vec<(String, String)>) -> Result<()> {
        self.ensure_clean()?;
        let head = self
            .repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .map(|o| o.to_string())
            .unwrap_or_default();
        let paths: Vec<String> = writes.iter().map(|(p, _)| p.clone()).collect();
        for path in &paths {
            validate_relative_path(path)?;
        }
        let marker = CrashTransaction {
            head,
            paths: paths.clone(),
            summary: summary.into(),
            expected_tree: None,
        };
        write_json_file(
            &self.repo.path().join("orchard-mail-transaction.json"),
            &marker,
        )?;

        let attempt = (|| -> Result<git2::Oid> {
            for (relative, contents) in &writes {
                let path = self.root.join(relative);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = File::create(path)?;
                file.write_all(contents.as_bytes())?;
                file.sync_all()?;
                if let Some(parent) = self.root.join(relative).parent() {
                    sync_dir(parent)?;
                }
            }
            let mut index = self.repo.index()?;
            for path in &paths {
                index.add_path(Path::new(path))?;
            }
            index.write()?;
            let tree_id = index.write_tree()?;
            let mut prepared = marker.clone();
            prepared.expected_tree = Some(tree_id.to_string());
            write_json_file(
                &self.repo.path().join("orchard-mail-transaction.json"),
                &prepared,
            )?;
            let tree = self.repo.find_tree(tree_id)?;
            let sig = Signature::now("Orchard Mail", "orchard-mail@localhost")?;
            let commit_id = match self.repo.head().ok().and_then(|h| h.target()) {
                Some(oid) => {
                    let parent = self.repo.find_commit(oid)?;
                    self.repo
                        .commit(Some("HEAD"), &sig, &sig, summary, &tree, &[&parent])?
                }
                None => self
                    .repo
                    .commit(Some("HEAD"), &sig, &sig, summary, &tree, &[])?,
            };
            Ok(commit_id)
        })();

        let commit_id = match attempt {
            Ok(commit_id) => commit_id,
            Err(error) => {
                let recovery = self.recover_known_transaction();
                return match recovery {
                    Ok(()) => Err(error),
                    Err(recovery) => {
                        self.poisoned = Some(recovery.to_string());
                        Err(MailError::Corrupt(format!(
                            "transaction failed ({error}); recovery failed ({recovery})"
                        )))
                    }
                };
            }
        };
        self.expected_head = Some(commit_id);
        #[cfg(test)]
        let injected = if std::mem::take(&mut self.fault_after_commit) {
            Some(MailError::Io(std::io::Error::other(
                "injected postcommit cleanup failure",
            )))
        } else {
            None
        };
        #[cfg(not(test))]
        let injected: Option<MailError> = None;
        let finalization = if let Some(error) = injected {
            Err(error)
        } else {
            sync_dir(self.repo.path())
                .and_then(|_| {
                    fs::remove_file(self.repo.path().join("orchard-mail-transaction.json"))
                        .map_err(MailError::from)
                })
                .and_then(|_| self.ensure_clean())
        };
        if let Err(error) = finalization {
            match self.recover_known_transaction() {
                Ok(()) => {
                    match self.rebuild_index() {
                        Ok(index) => self.index = index,
                        Err(rebuild) => {
                            self.poisoned = Some(rebuild.to_string());
                            return Err(MailError::Corrupt(format!(
                                "commit succeeded but index rebuild failed: {rebuild}"
                            )));
                        }
                    }
                    return Err(error);
                }
                Err(recovery) => {
                    self.poisoned = Some(recovery.to_string());
                    return Err(MailError::Corrupt(format!("commit succeeded but finalization failed ({error}); recovery failed ({recovery})")));
                }
            }
        }
        Ok(())
    }

    fn ensure_clean(&self) -> Result<()> {
        if let Some(expected) = self.expected_head {
            let actual = self
                .repo
                .head()?
                .target()
                .ok_or_else(|| MailError::Corrupt("HEAD has no direct target".into()))?;
            if actual != expected {
                return Err(MailError::Dirty(format!(
                    "HEAD moved outside this MailService (expected {expected}, found {actual})"
                )));
            }
        }
        let dirty = self.status_paths()?;
        if dirty.is_empty() {
            Ok(())
        } else {
            Err(MailError::Dirty(dirty.join(", ")))
        }
    }

    fn status_paths(&self) -> Result<Vec<String>> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false);
        let statuses = self.repo.statuses(Some(&mut opts))?;
        Ok(statuses
            .iter()
            .filter_map(|s| s.path().map(str::to_owned))
            .collect())
    }

    fn recover_known_transaction(&mut self) -> Result<()> {
        let marker_path = self.repo.path().join("orchard-mail-transaction.json");
        if !marker_path.exists() {
            return Ok(());
        }
        let marker: CrashTransaction = serde_json::from_slice(&fs::read(&marker_path)?)
            .map_err(|e| MailError::Corrupt(format!("invalid transaction marker: {e}")))?;
        let current = self
            .repo
            .head()
            .ok()
            .and_then(|h| h.target())
            .map(|o| o.to_string())
            .unwrap_or_default();
        if marker.head != current {
            let committed = (|| -> Result<bool> {
                let oid = self
                    .repo
                    .head()?
                    .target()
                    .ok_or_else(|| MailError::Corrupt("HEAD has no target".into()))?;
                let commit = self.repo.find_commit(oid)?;
                let parent_matches = if marker.head.is_empty() {
                    commit.parent_count() == 0
                } else {
                    commit.parent_count() == 1 && commit.parent_id(0)?.to_string() == marker.head
                };
                let tree_matches =
                    marker.expected_tree.as_deref() == Some(&commit.tree_id().to_string());
                Ok(parent_matches
                    && tree_matches
                    && commit.message() == Some(marker.summary.as_str())
                    && self.status_paths()?.is_empty())
            })()?;
            if committed {
                fs::remove_file(marker_path)?;
                return Ok(());
            }
            return Err(MailError::Corrupt(
                "transaction marker does not describe the current HEAD commit".into(),
            ));
        }
        let allowed: BTreeSet<_> = marker.paths.iter().cloned().collect();
        let dirty = self.status_paths()?;
        if dirty.iter().any(|p| !allowed.contains(p)) {
            return Err(MailError::Dirty(format!(
                "crash marker does not own: {}",
                dirty.join(", ")
            )));
        }
        let head_commit = self.repo.head()?.peel_to_commit()?;
        let head_tree = head_commit.tree()?;
        for path in &marker.paths {
            validate_relative_path(path)?;
            if head_tree.get_path(Path::new(path)).is_ok() {
                let mut checkout = CheckoutBuilder::new();
                checkout.force().path(path);
                self.repo.checkout_head(Some(&mut checkout))?;
            } else {
                let absolute = self.root.join(path);
                if absolute.exists() {
                    fs::remove_file(absolute)?;
                }
            }
        }
        let mut index = self.repo.index()?;
        index.read_tree(&head_tree)?;
        index.write()?;
        fs::remove_file(marker_path)?;
        self.ensure_clean()
    }

    fn rebuild_index(&self) -> Result<MailIndex> {
        let mut index = MailIndex {
            next_sequence: 1,
            ..MailIndex::default()
        };
        self.verify_git_history()?;
        self.verify_tracked_layout()?;
        for (path, value) in
            read_markdown_dir::<StoredParticipant>(&self.root.join("participants"))?
        {
            verify_filename_id(&path, &value.id)?;
            validate_id("participant id", &value.id)?;
            if index.participants.insert(value.id.clone(), value).is_some() {
                return Err(MailError::Corrupt("duplicate participant id".into()));
            }
        }
        for (path, value) in read_markdown_dir::<Channel>(&self.root.join("channels"))? {
            verify_filename_id(&path, &value.id)?;
            validate_id("channel id", &value.id)?;
            if index.channels.insert(value.id.clone(), value).is_some() {
                return Err(MailError::Corrupt("duplicate channel id".into()));
            }
        }
        for (path, value) in read_markdown_dir::<Message>(&self.root.join("messages"))? {
            verify_filename_id(&path, &value.id)?;
            if value.id != format!("m_{:020}", value.sequence) {
                return Err(MailError::Corrupt(format!(
                    "message {} sequence mismatch",
                    value.id
                )));
            }
            index.messages.push(value);
        }
        index.messages.sort_by_key(|m| m.sequence);
        if index
            .messages
            .windows(2)
            .any(|pair| pair[0].sequence == pair[1].sequence)
        {
            return Err(MailError::Corrupt("duplicate message sequence".into()));
        }
        for (path, value) in read_markdown_dir::<AckRecord>(&self.root.join("acknowledgements"))? {
            validate_id(
                "acknowledgement filename",
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default(),
            )?;
            index
                .acknowledgements
                .entry(value.participant_id)
                .or_default()
                .extend(value.message_ids);
        }
        for (path, value) in read_markdown_dir::<RequestRecord>(&self.root.join("requests"))? {
            verify_filename_id(&path, &value.request_id)?;
            validate_request_id(&value.request_id)?;
            if index
                .requests
                .insert(value.request_id.clone(), value)
                .is_some()
            {
                return Err(MailError::Corrupt("duplicate request id".into()));
            }
        }
        let state_path = self.root.join("state.md");
        let state: StateRecord = parse_markdown(&fs::read_to_string(&state_path)?)
            .map_err(|e| MailError::Corrupt(format!("{}: {e}", state_path.display())))?;
        index.next_sequence = state.next_sequence;
        if !index.channels.contains_key("general") {
            return Err(MailError::Corrupt("missing general channel".into()));
        }
        if index
            .messages
            .last()
            .is_some_and(|m| m.sequence >= index.next_sequence)
        {
            return Err(MailError::Corrupt(
                "next_sequence does not follow messages".into(),
            ));
        }
        for message in &index.messages {
            if index
                .requests
                .get(&message.request_id)
                .is_none_or(|request| request.operation != "mail_send")
            {
                return Err(MailError::Corrupt(format!(
                    "message {} has no matching send request",
                    message.id
                )));
            }
            if !index.participants.contains_key(&message.sender_id) {
                return Err(MailError::Corrupt(format!(
                    "message {} has unknown sender",
                    message.id
                )));
            }
            if message
                .recipient_ids
                .iter()
                .any(|id| !index.participants.contains_key(id))
            {
                return Err(MailError::Corrupt(format!(
                    "message {} has unknown recipient",
                    message.id
                )));
            }
            match &message.destination {
                Destination::Channel { id } if !index.channels.contains_key(id) => {
                    return Err(MailError::Corrupt(format!(
                        "message {} has unknown channel",
                        message.id
                    )))
                }
                Destination::Direct { id } if !index.participants.contains_key(id) => {
                    return Err(MailError::Corrupt(format!(
                        "message {} has unknown direct recipient",
                        message.id
                    )))
                }
                _ => {}
            }
            if message.thread_id.as_ref().is_some_and(|thread_id| {
                !index
                    .messages
                    .iter()
                    .any(|candidate| &candidate.id == thread_id)
            }) {
                return Err(MailError::Corrupt(format!(
                    "message {} references an unknown thread message",
                    message.id
                )));
            }
        }
        Ok(index)
    }

    fn verify_tracked_layout(&self) -> Result<()> {
        let head = self.repo.head()?.peel_to_commit()?;
        let tree = head.tree()?;
        let mut seen = BTreeSet::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob) {
                if let Some(name) = entry.name() {
                    seen.insert(format!("{root}{name}"));
                }
            }
            git2::TreeWalkResult::Ok
        })?;
        for path in &seen {
            let allowed = path == "state.md"
                || [
                    "participants/",
                    "channels/",
                    "messages/",
                    "acknowledgements/",
                    "requests/",
                ]
                .iter()
                .any(|prefix| path.starts_with(prefix) && path.ends_with(".md"));
            if !allowed {
                return Err(MailError::Corrupt(format!(
                    "unexpected tracked path {path}"
                )));
            }
            let absolute = self.root.join(path);
            let metadata = fs::symlink_metadata(&absolute)?;
            if !metadata.file_type().is_file() {
                return Err(MailError::Corrupt(format!(
                    "record is not a regular file: {path}"
                )));
            }
            let entry = tree.get_path(Path::new(path))?;
            let blob = self.repo.find_blob(entry.id())?;
            if blob.content() != fs::read(&absolute)?.as_slice() {
                return Err(MailError::Corrupt(format!(
                    "record does not match HEAD blob: {path}"
                )));
            }
        }
        Ok(())
    }

    fn verify_git_history(&self) -> Result<()> {
        let mut commit = self.repo.head()?.peel_to_commit()?;
        loop {
            for signature in [commit.author(), commit.committer()] {
                if signature.name() != Some("Orchard Mail")
                    || signature.email() != Some("orchard-mail@localhost")
                {
                    return Err(MailError::Corrupt(format!(
                        "commit {} has an unexpected identity",
                        commit.id()
                    )));
                }
            }
            if commit.parent_count() == 0 {
                if commit.message() != Some("Initialize Orchard Mail") {
                    return Err(MailError::Corrupt(
                        "root commit is not Orchard Mail initialization".into(),
                    ));
                }
                let paths = tree_blob_paths(&commit.tree()?)?;
                if paths
                    != BTreeSet::from(["channels/general.md".to_owned(), "state.md".to_owned()])
                {
                    return Err(MailError::Corrupt(
                        "initial commit has unexpected records".into(),
                    ));
                }
                break;
            }
            if commit.parent_count() != 1 {
                return Err(MailError::Corrupt(format!(
                    "merge commit {} is not allowed",
                    commit.id()
                )));
            }
            let summary = commit.message().ok_or_else(|| {
                MailError::Corrupt(format!("commit {} has no UTF-8 message", commit.id()))
            })?;
            let (operation, request_id) = summary.split_once(' ').ok_or_else(|| {
                MailError::Corrupt(format!("commit {} has invalid summary", commit.id()))
            })?;
            validate_request_id(request_id).map_err(|e| MailError::Corrupt(e.to_string()))?;
            let parent = commit.parent(0)?;
            let parent_tree = parent.tree()?;
            let tree = commit.tree()?;
            let diff = self
                .repo
                .diff_tree_to_tree(Some(&parent_tree), Some(&tree), None)?;
            let mut changed = BTreeMap::new();
            for delta in diff.deltas() {
                if !matches!(delta.status(), git2::Delta::Added | git2::Delta::Modified) {
                    return Err(MailError::Corrupt(format!(
                        "commit {} deletes or renames a record",
                        commit.id()
                    )));
                }
                let path = delta
                    .new_file()
                    .path()
                    .and_then(Path::to_str)
                    .ok_or_else(|| MailError::Corrupt("non-UTF-8 Git path".into()))?
                    .to_owned();
                changed.insert(path, delta.status());
            }
            let request_path = format!("requests/{request_id}.md");
            if changed.get(&request_path) != Some(&git2::Delta::Added) {
                return Err(MailError::Corrupt(format!(
                    "commit {} lacks its request record",
                    commit.id()
                )));
            }
            let shape_ok = match operation {
                "mail_register" => {
                    changed.len() == 2
                        && changed
                            .iter()
                            .filter(|(path, status)| {
                                path.starts_with("participants/")
                                    && matches!(status, git2::Delta::Added | git2::Delta::Modified)
                            })
                            .count()
                            == 1
                }
                "mail_leave" => {
                    changed.len() == 2
                        && changed
                            .iter()
                            .filter(|(path, status)| {
                                path.starts_with("participants/")
                                    && **status == git2::Delta::Modified
                            })
                            .count()
                            == 1
                }
                "mail_channel_create" => {
                    changed.len() == 2
                        && changed
                            .iter()
                            .filter(|(path, status)| {
                                path.starts_with("channels/") && **status == git2::Delta::Added
                            })
                            .count()
                            == 1
                }
                "mail_send" => {
                    changed.len() == 3
                        && changed.get("state.md") == Some(&git2::Delta::Modified)
                        && changed
                            .iter()
                            .filter(|(path, status)| {
                                path.starts_with("messages/") && **status == git2::Delta::Added
                            })
                            .count()
                            == 1
                }
                "mail_acknowledge" => {
                    changed.len() == 2
                        && changed.get(&format!("acknowledgements/{request_id}.md"))
                            == Some(&git2::Delta::Added)
                }
                _ => {
                    return Err(MailError::Corrupt(format!(
                        "commit {} names unsupported mutation {operation}",
                        commit.id()
                    )))
                }
            };
            if !shape_ok {
                return Err(MailError::Corrupt(format!(
                    "commit {} changes paths outside {operation}",
                    commit.id()
                )));
            }
            let request_entry = tree.get_path(Path::new(&request_path))?;
            let request_blob = self.repo.find_blob(request_entry.id())?;
            let request_text = std::str::from_utf8(request_blob.content())
                .map_err(|_| MailError::Corrupt(format!("{request_path} is not UTF-8")))?;
            let request: RequestRecord = parse_markdown(request_text)?;
            if request.request_id != request_id
                || request.operation != operation
                || request.fingerprint.len() != 64
                || !request.fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(MailError::Corrupt(format!(
                    "commit {} request record does not match its identity",
                    commit.id()
                )));
            }
            if operation == "mail_send" {
                let message_path = changed
                    .keys()
                    .find(|path| path.starts_with("messages/"))
                    .ok_or_else(|| MailError::Corrupt("send commit lacks message path".into()))?;
                let message_entry = tree.get_path(Path::new(message_path))?;
                let message_blob = self.repo.find_blob(message_entry.id())?;
                let message_text = std::str::from_utf8(message_blob.content())
                    .map_err(|_| MailError::Corrupt(format!("{message_path} is not UTF-8")))?;
                let message: Message = parse_markdown(message_text)?;
                if message.request_id != request_id {
                    return Err(MailError::Corrupt(format!(
                        "commit {} message request_id does not match",
                        commit.id()
                    )));
                }
            }
            commit = parent;
        }
        Ok(())
    }
}

fn public_participant(stored: &StoredParticipant, session: Option<SessionPresence>) -> Participant {
    Participant {
        id: stored.id.clone(),
        name: stored.name.clone(),
        registered_at: stored.registered_at,
        registered: stored.registered,
        session_instance_id: session.as_ref().and_then(|s| s.id.clone()),
        last_contact_at: session.map(|s| s.last_contact_at),
    }
}

fn bounded_limit(limit: Option<usize>) -> Result<usize> {
    let n = limit.unwrap_or(50);
    if n == 0 || n > 200 {
        Err(MailError::Invalid("limit must be between 1 and 200".into()))
    } else {
        Ok(n)
    }
}

fn validate_id(label: &str, id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        Err(MailError::Invalid(format!(
            "{label} must contain only ASCII letters, numbers, dot, underscore, or hyphen"
        )))
    } else {
        Ok(())
    }
}

fn validate_request_id(id: &str) -> Result<()> {
    validate_id("request_id", id)
}

fn validate_relative_path(path: &str) -> Result<()> {
    let candidate = Path::new(path);
    if candidate.is_absolute()
        || candidate
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(MailError::Corrupt(format!(
            "unsafe transaction path {path}"
        )));
    }
    Ok(())
}

fn slug(name: &str) -> String {
    let value = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let compact = value
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if compact.is_empty() {
        format!("channel-{}", Uuid::new_v4().simple())
    } else {
        compact
    }
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| MailError::Invalid(e.to_string()))
}

fn markdown<T: Serialize>(metadata: &T, text: &str) -> Result<String> {
    Ok(format!(
        "<!-- orchard-mail:v1\n{}\n-->\n\n{}\n",
        serde_json::to_string_pretty(metadata)?,
        text
    ))
}

fn parse_markdown<T: DeserializeOwned>(contents: &str) -> Result<T> {
    let prefix = "<!-- orchard-mail:v1\n";
    if !contents.starts_with(prefix) {
        return Err(MailError::Corrupt("missing orchard-mail:v1 header".into()));
    }
    let rest = &contents[prefix.len()..];
    let end = rest
        .find("\n-->")
        .ok_or_else(|| MailError::Corrupt("unterminated metadata header".into()))?;
    serde_json::from_str(&rest[..end]).map_err(MailError::from)
}

fn read_markdown_dir<T: DeserializeOwned>(dir: &Path) -> Result<Vec<(PathBuf, T)>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<_> = fs::read_dir(dir)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    paths.sort();
    paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .map(|path| {
            let text = fs::read_to_string(&path)?;
            let value = parse_markdown(&text)
                .map_err(|e| MailError::Corrupt(format!("{}: {e}", path.display())))?;
            Ok((path, value))
        })
        .collect()
}

fn fingerprint(operation: &str, args: &Value) -> Result<String> {
    let canonical = canonicalize(args);
    let bytes = serde_json::to_vec(&canonical)?;
    let mut hash = Sha256::new();
    hash.update(operation.as_bytes());
    hash.update([0]);
    hash.update(bytes);
    Ok(hex::encode(hash.finalize()))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map
                .iter()
                .map(|(k, v)| (k.clone(), canonicalize(v)))
                .collect();
            Value::Object(sorted.into_iter().collect::<Map<_, _>>())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

fn write_json_file(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = File::create(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

fn verify_filename_id(path: &Path, id: &str) -> Result<()> {
    if path.file_stem().and_then(|s| s.to_str()) == Some(id) {
        Ok(())
    } else {
        Err(MailError::Corrupt(format!(
            "{} metadata id does not match filename",
            path.display()
        )))
    }
}

fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn tree_blob_paths(tree: &git2::Tree<'_>) -> Result<BTreeSet<String>> {
    let mut paths = BTreeSet::new();
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            if let Some(name) = entry.name() {
                paths.insert(format!("{root}{name}"));
            }
        }
        git2::TreeWalkResult::Ok
    })?;
    Ok(paths)
}

#[cfg(test)]
mod fault_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn postcommit_failure_rehydrates_for_retry_and_next_sequence() {
        let temp = TempDir::new().unwrap();
        let mut mail = MailService::open(temp.path()).unwrap();
        mail.call(
            "mail_register",
            json!({"request_id":"ra","name":"Alice","participant_id":"alice"}),
        )
        .unwrap();
        mail.call(
            "mail_register",
            json!({"request_id":"rb","name":"Bob","participant_id":"bob"}),
        )
        .unwrap();
        let first_args = json!({
            "request_id":"s1", "sender_id":"alice", "destination":{"kind":"direct","id":"bob"}, "body":"one"
        });
        mail.fault_after_commit = true;
        assert!(matches!(
            mail.call("mail_send", first_args.clone()),
            Err(MailError::Io(_))
        ));
        let retry = mail.call("mail_send", first_args).unwrap();
        assert_eq!(retry["message"]["sequence"], 1);
        let next = mail.call("mail_send", json!({
            "request_id":"s2", "sender_id":"alice", "destination":{"kind":"direct","id":"bob"}, "body":"two"
        })).unwrap();
        assert_eq!(next["message"]["sequence"], 2);
        assert!(temp
            .path()
            .join("messages/m_00000000000000000001.md")
            .exists());
        assert!(temp
            .path()
            .join("messages/m_00000000000000000002.md")
            .exists());
    }
}
