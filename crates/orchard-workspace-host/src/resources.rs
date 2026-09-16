use base64::Engine;
use git2::{Commit, ObjectType, Oid, Repository, Signature, Tree};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::{object, required_string, WorkspaceConfig, WorkspaceHost};

const MAX_UPLOAD_BYTES: usize = 512 * 1024;
const MAX_READ_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEXT_PREVIEW_BYTES: usize = 128 * 1024;
const MAX_LIST_ENTRIES: usize = 500;
const MAX_HISTORY_VERSIONS: usize = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResourceKind {
    Channel,
    Direct,
    Broadcast,
    Message,
    Agent,
    Task,
    File,
    Url,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct ResourceRef {
    pub kind: ResourceKind,
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone)]
struct ArtifactRoot {
    id: String,
    name: String,
    path: PathBuf,
    owned: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct UploadReceipt {
    path: String,
    fingerprint: String,
}

pub(crate) struct ArtifactDownload {
    pub bytes: Vec<u8>,
    pub filename: String,
    pub preview_mime: Option<&'static str>,
}

impl WorkspaceHost {
    pub(crate) fn resource_get(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let reference = parse_ref_arg(&args, "ref", &workspace_id)?;
        self.resource_get_ref(&workspace_id, reference)
    }

    pub(crate) fn resource_get_href(
        &self,
        workspace_id: &str,
        href: &str,
    ) -> Result<Value, String> {
        let reference = parse_href(href)?;
        if reference.workspace_id != workspace_id {
            return Err("resource href belongs to a different workspace".to_owned());
        }
        self.resource_get_ref(workspace_id, reference)
    }

    fn resource_get_ref(
        &self,
        workspace_id: &str,
        reference: ResourceRef,
    ) -> Result<Value, String> {
        validate_ref(&reference, workspace_id)?;
        self.active_runtime(workspace_id)?;
        let href = format_href(&reference)?;
        let (title, data) = match reference.kind {
            ResourceKind::Channel => {
                let id = required_ref(&reference.id, "id")?;
                let channels = self.mail_read(workspace_id, "mail_channels", json!({}))?;
                let channel = channels["channels"]
                    .as_array()
                    .and_then(|items| items.iter().find(|item| item["id"] == id))
                    .cloned()
                    .ok_or_else(|| format!("unknown channel {id:?}"))?;
                let messages = self
                    .all_messages(workspace_id)?
                    .into_iter()
                    .filter(|message| {
                        message.pointer("/destination/kind") == Some(&json!("channel"))
                            && message.pointer("/destination/id") == Some(&json!(id))
                    })
                    .collect::<Vec<_>>();
                (
                    channel["name"].as_str().unwrap_or(id).to_owned(),
                    json!({"channel":channel,"messages":messages}),
                )
            }
            ResourceKind::Direct => {
                let id = required_ref(&reference.id, "id")?;
                let participants = self.mail_read(workspace_id, "mail_participants", json!({}))?;
                let participant = participants["participants"]
                    .as_array()
                    .and_then(|items| items.iter().find(|item| item["id"] == id))
                    .cloned()
                    .ok_or_else(|| format!("unknown participant {id:?}"))?;
                let messages = self
                    .all_messages(workspace_id)?
                    .into_iter()
                    .filter(|message| {
                        message.pointer("/destination/kind") == Some(&json!("direct"))
                            && ((message["sender_id"] == "owner"
                                && message.pointer("/destination/id") == Some(&json!(id)))
                                || (message["sender_id"] == id
                                    && message.pointer("/destination/id") == Some(&json!("owner"))))
                    })
                    .collect::<Vec<_>>();
                (
                    participant["name"].as_str().unwrap_or(id).to_owned(),
                    json!({"participant":participant,"messages":messages}),
                )
            }
            ResourceKind::Broadcast => {
                let messages = self
                    .all_messages(workspace_id)?
                    .into_iter()
                    .filter(|message| {
                        message.pointer("/destination/kind") == Some(&json!("broadcast"))
                    })
                    .collect::<Vec<_>>();
                ("Broadcast".to_owned(), json!({"messages":messages}))
            }
            ResourceKind::Message => {
                let id = required_ref(&reference.id, "id")?;
                let message = self
                    .all_messages(workspace_id)?
                    .into_iter()
                    .find(|message| message["id"] == id)
                    .ok_or_else(|| format!("unknown message {id:?}"))?;
                (format!("Message {id}"), json!({"message":message}))
            }
            ResourceKind::Agent => {
                let id = required_ref(&reference.id, "id")?;
                let participants = self.mail_read(workspace_id, "mail_participants", json!({}))?;
                let participant = participants["participants"]
                    .as_array()
                    .and_then(|items| items.iter().find(|item| item["id"] == id))
                    .cloned()
                    .ok_or_else(|| format!("unknown participant {id:?}"))?;
                (
                    participant["name"].as_str().unwrap_or(id).to_owned(),
                    json!({"participant":participant}),
                )
            }
            ResourceKind::Task => {
                let store_id = required_ref(&reference.store_id, "store_id")?;
                let task_id = required_ref(&reference.task_id, "task_id")?;
                let task = self.task_show(json!({
                    "workspace_id":workspace_id,"store_id":store_id,"task_id":task_id
                }))?;
                let dependencies = self.task_dependencies(json!({
                    "workspace_id":workspace_id,"store_id":store_id,"task_id":task_id
                }))?;
                (
                    task["title"].as_str().unwrap_or(task_id).to_owned(),
                    json!({"task":task,"dependencies":dependencies["dependencies"]}),
                )
            }
            ResourceKind::File => {
                let root_id = required_ref(&reference.root_id, "root_id")?;
                let path = required_ref(&reference.path, "path")?;
                let (bytes, revision) =
                    self.read_artifact(workspace_id, root_id, path, reference.revision.as_deref())?;
                let binary = std::str::from_utf8(&bytes).is_err() || bytes.contains(&0);
                let text = (!binary && bytes.len() <= MAX_TEXT_PREVIEW_BYTES)
                    .then(|| String::from_utf8(bytes.clone()).expect("checked UTF-8"));
                let download_url = download_href(workspace_id, root_id, path, revision.as_deref());
                let mime_type = safe_preview_mime(&bytes);
                let preview_url = mime_type
                    .map(|_| preview_href(workspace_id, root_id, path, revision.as_deref()));
                (
                    Path::new(path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(path)
                        .to_owned(),
                    json!({
                        "text":text,"binary":binary,"byte_length":bytes.len(),
                        "revision":revision,"download_url":download_url,
                        "mime_type":mime_type,"preview_url":preview_url
                    }),
                )
            }
            ResourceKind::Url => {
                let url = required_ref(&reference.url, "url")?;
                validate_url(url)?;
                (url.to_owned(), json!({"url":url}))
            }
        };
        let links = self.links_for(workspace_id, &reference)?;
        Ok(json!({
            "resource":{"ref":reference,"href":href,"title":title,"kind":kind_name(&reference.kind),"data":data},
            "links":links
        }))
    }

    pub(crate) fn resource_links(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let reference = parse_ref_arg(&args, "ref", &workspace_id)?;
        validate_ref(&reference, &workspace_id)?;
        self.links_for(&workspace_id, &reference)
    }

    pub(crate) fn resource_link(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let source = parse_ref_arg(&args, "source", &workspace_id)?;
        let target = parse_ref_arg(&args, "target", &workspace_id)?;
        validate_ref(&source, &workspace_id)?;
        validate_ref(&target, &workspace_id)?;
        let request_id = required_string(&args, "request_id")?;
        let label = args.get("label").and_then(Value::as_str).map(str::to_owned);
        let runtime = self.active_runtime(&workspace_id)?;
        let mail = runtime
            .mail
            .as_ref()
            .ok_or_else(|| "mail unavailable".to_owned())?;
        let mut mail = mail.lock().unwrap();
        let _ = mail.call(
            "mail_channel_create",
            json!({
                "request_id":"orchard-resource-links-channel-v1","channel_id":"orchard-system",
                "name":"Orchard system","description":"Immutable Orchard resource links"
            }),
        );
        let reference = json!({
            "type":"orchard_resource_link","source":source,"target":target,"label":label
        });
        let result = mail
            .call(
                "mail_send",
                json!({
                    "request_id":request_id,"sender_id":"orchard",
                    "destination":{"kind":"channel","id":"orchard-system"},
                    "body":"Orchard resource link","kind":"resource_link","refs":[reference]
                }),
            )
            .map_err(|error| error.to_string())?;
        Ok(json!({"link":result["message"]["refs"][0]}))
    }

    pub(crate) fn artifact_roots(&self, args: Value) -> Result<Value, String> {
        let workspace_id = crate::workspace_id(&args)?;
        self.active_runtime(&workspace_id)?;
        let workspace = self.workspace_config(&workspace_id)?;
        let roots = artifact_roots_for(&workspace)
            .into_iter()
            .map(|root| json!({"id":root.id,"name":root.name,"owned":root.owned,"exists":root.path.exists()}))
            .collect::<Vec<_>>();
        Ok(json!({"roots":roots}))
    }

    pub(crate) fn artifact_list(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        self.active_runtime(&workspace_id)?;
        let root_id = required_string(&args, "root_id")?;
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        validate_relative_path(path, true)?;
        let revision = args.get("revision").and_then(Value::as_str);
        let workspace = self.workspace_config(&workspace_id)?;
        let root = find_root(&workspace, &root_id)?;
        let repo = open_root_repo(&root)?;
        let resolved_revision = revision.map(parse_oid).transpose()?;
        let entries = if let Some(oid) = resolved_revision {
            list_tree(&repo, oid, path)?
        } else if repo.is_bare() {
            let oid = repo
                .head()
                .and_then(|head| head.peel_to_commit())
                .map_err(git_error)?
                .id();
            list_tree(&repo, oid, path)?
        } else {
            list_live_index(&repo, path)?
        };
        let truncated = entries.len() > MAX_LIST_ENTRIES;
        let entries = entries
            .into_iter()
            .take(MAX_LIST_ENTRIES)
            .map(|(name, full_path, kind)| {
                let reference = (kind == "file").then(|| ResourceRef {
                    kind: ResourceKind::File,
                    workspace_id: workspace_id.clone(),
                    id: None,
                    store_id: None,
                    task_id: None,
                    root_id: Some(root_id.clone()),
                    path: Some(full_path.clone()),
                    revision: resolved_revision.map(|oid| oid.to_string()),
                    url: None,
                });
                let href = reference
                    .as_ref()
                    .and_then(|reference| format_href(reference).ok());
                json!({"name":name,"path":full_path,"kind":kind,"ref":reference,"href":href})
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "root":{"id":root.id,"name":root.name,"owned":root.owned,"exists":root.path.exists()},
            "path":path,"revision":resolved_revision.map(|oid| oid.to_string()),"entries":entries,
            "truncated":truncated
        }))
    }

    pub(crate) fn artifact_history(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        self.active_runtime(&workspace_id)?;
        let root_id = required_string(&args, "root_id")?;
        let path = required_string(&args, "path")?;
        validate_relative_path(&path, false)?;
        let workspace = self.workspace_config(&workspace_id)?;
        let root = find_root(&workspace, &root_id)?;
        let repo = open_root_repo(&root)?;
        let mut walk = repo.revwalk().map_err(git_error)?;
        walk.push_head().map_err(git_error)?;
        walk.set_sorting(git2::Sort::TIME).map_err(git_error)?;
        let mut versions = Vec::new();
        for oid in walk {
            let commit = repo
                .find_commit(oid.map_err(git_error)?)
                .map_err(git_error)?;
            let current = tree_entry_identity(&commit.tree().map_err(git_error)?, &path);
            if current.is_none() {
                continue;
            }
            let parent = commit
                .parent(0)
                .ok()
                .and_then(|parent| parent.tree().ok())
                .and_then(|tree| tree_entry_identity(&tree, &path));
            if current == parent {
                continue;
            }
            versions.push(json!({
                "revision":commit.id().to_string(),
                "summary":commit.summary().unwrap_or("Artifact update"),
                "author":commit.author().name().unwrap_or("Unknown"),
                "committed_at":commit.time().seconds()
            }));
            if versions.len() == MAX_HISTORY_VERSIONS {
                break;
            }
        }
        Ok(
            json!({"root_id":root_id,"path":path,"versions":versions,"truncated":versions.len() == MAX_HISTORY_VERSIONS}),
        )
    }

    pub(crate) fn artifact_upload(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        self.active_runtime(&workspace_id)?;
        let path = required_string(&args, "path")?;
        validate_relative_path(&path, false)?;
        let request_id = required_string(&args, "request_id")?;
        validate_request_id(&request_id)?;
        let encoded = required_string(&args, "content_base64")?;
        let content = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| "content_base64 is not valid base64".to_owned())?;
        if content.len() > MAX_UPLOAD_BYTES {
            return Err(format!("artifact upload exceeds {MAX_UPLOAD_BYTES} bytes"));
        }
        let fingerprint = format!("{:x}", Sha256::digest(&content));
        let _guard = self.inner.artifact_lock.lock().unwrap();
        let workspace = self.workspace_config(&workspace_id)?;
        let root = find_root(&workspace, "artifacts")?;
        create_owned_root(&workspace.root, &root.path)?;
        let git_dir = root.path.join(".git");
        let repo = if git_dir.exists() {
            validate_owned_git_metadata(&root.path, &git_dir)?;
            Repository::open(&root.path).map_err(git_error)?
        } else {
            Repository::init(&root.path).map_err(git_error)?
        };
        validate_owned_repository(&root.path, &repo)?;
        let receipt_path = format!(".orchard/requests/{request_id}.json");
        if let Some(revision) = find_upload_commit(&repo, &receipt_path, &path, &fingerprint)? {
            return self.upload_result(&workspace_id, &path, revision);
        }
        let target = root.path.join(&path);
        let receipt_file = root.path.join(&receipt_path);
        ensure_safe_live_path(&root.path, &path, false)?;
        ensure_safe_live_path(&root.path, &receipt_path, false)?;
        let statuses = repo.statuses(None).map_err(git_error)?;
        if !statuses.is_empty() {
            let pending = read_pending_receipt(&receipt_file, &path, &fingerprint)?;
            let only_this_upload = statuses.iter().all(|status| {
                status
                    .path()
                    .is_some_and(|changed| changed == path || changed == receipt_path)
            });
            if pending && only_this_upload {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                ensure_safe_live_path(&root.path, &path, false)?;
                fs::write(&target, &content).map_err(|error| error.to_string())?;
                let revision = commit_paths(&repo, &[&path, &receipt_path], &request_id)?;
                return self.upload_result(&workspace_id, &path, revision);
            }
            return Err(
                "the owned artifact repository has uncommitted changes; refusing upload".to_owned(),
            );
        }
        if target.exists()
            && repo
                .index()
                .map_err(git_error)?
                .get_path(Path::new(&path), 0)
                .is_none()
        {
            return Err("refusing to overwrite an untracked artifact".to_owned());
        }
        let receipt = UploadReceipt {
            path: path.clone(),
            fingerprint,
        };
        if let Some(parent) = receipt_file.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        ensure_safe_live_path(&root.path, &receipt_path, false)?;
        fs::write(
            &receipt_file,
            serde_json::to_vec_pretty(&receipt).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        ensure_safe_live_path(&root.path, &path, false)?;
        fs::write(&target, &content).map_err(|error| error.to_string())?;
        let revision = commit_paths(&repo, &[&path, &receipt_path], &request_id)?;
        self.upload_result(&workspace_id, &path, revision)
    }

    fn upload_result(
        &self,
        workspace_id: &str,
        path: &str,
        revision: Oid,
    ) -> Result<Value, String> {
        let reference = ResourceRef {
            kind: ResourceKind::File,
            workspace_id: workspace_id.to_owned(),
            id: None,
            store_id: None,
            task_id: None,
            root_id: Some("artifacts".to_owned()),
            path: Some(path.to_owned()),
            revision: Some(revision.to_string()),
            url: None,
        };
        let href = format_href(&reference)?;
        Ok(
            json!({"resource":{"ref":reference,"href":href,"title":Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path),"kind":"file"},"revision":revision.to_string()}),
        )
    }

    pub(crate) fn artifact_download(
        &self,
        workspace_id: &str,
        root_id: &str,
        path: &str,
        revision: Option<&str>,
    ) -> Result<ArtifactDownload, String> {
        let (bytes, _) = self.read_artifact(workspace_id, root_id, path, revision)?;
        Ok(ArtifactDownload {
            preview_mime: safe_preview_mime(&bytes),
            bytes,
            filename: Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("artifact")
                .to_owned(),
        })
    }

    fn read_artifact(
        &self,
        workspace_id: &str,
        root_id: &str,
        path: &str,
        revision: Option<&str>,
    ) -> Result<(Vec<u8>, Option<String>), String> {
        self.active_runtime(workspace_id)?;
        validate_relative_path(path, false)?;
        let workspace = self.workspace_config(workspace_id)?;
        let root = find_root(&workspace, root_id)?;
        let repo = open_root_repo(&root)?;
        let bytes = if revision.is_some() || repo.is_bare() {
            let oid = match revision {
                Some(revision) => parse_oid(revision)?,
                None => repo
                    .head()
                    .and_then(|head| head.peel_to_commit())
                    .map_err(git_error)?
                    .id(),
            };
            let commit = repo.find_commit(oid).map_err(git_error)?;
            let tree = commit.tree().map_err(git_error)?;
            let entry = tree.get_path(Path::new(path)).map_err(git_error)?;
            if entry.kind() != Some(ObjectType::Blob) || entry.filemode() == 0o120000 {
                return Err("artifact is not a regular file".to_owned());
            }
            let blob = repo.find_blob(entry.id()).map_err(git_error)?;
            if blob.size() > MAX_READ_BYTES {
                return Err(format!(
                    "artifact exceeds the {MAX_READ_BYTES} byte read limit"
                ));
            }
            blob.content().to_vec()
        } else {
            let index = repo.index().map_err(git_error)?;
            let entry = index
                .get_path(Path::new(path), 0)
                .ok_or_else(|| "artifact is not tracked".to_owned())?;
            if entry.mode == 0o120000 || entry.mode == 0o160000 {
                return Err("symlink and submodule artifacts cannot be opened".to_owned());
            }
            ensure_safe_live_path(&root.path, path, true)?;
            let target = root.path.join(path);
            let metadata = fs::symlink_metadata(&target).map_err(|error| error.to_string())?;
            if !metadata.file_type().is_file() {
                return Err("artifact is not a regular file".to_owned());
            }
            if metadata.len() > MAX_READ_BYTES as u64 {
                return Err(format!(
                    "artifact exceeds the {MAX_READ_BYTES} byte read limit"
                ));
            }
            fs::read(target).map_err(|error| error.to_string())?
        };
        if bytes.len() > MAX_READ_BYTES {
            return Err(format!(
                "artifact exceeds the {MAX_READ_BYTES} byte read limit"
            ));
        }
        Ok((bytes, revision.map(str::to_owned)))
    }

    fn mail_read(&self, workspace_id: &str, operation: &str, args: Value) -> Result<Value, String> {
        let runtime = self.active_runtime(workspace_id)?;
        let result = runtime
            .mail
            .as_ref()
            .ok_or_else(|| "mail unavailable".to_owned())?
            .lock()
            .unwrap()
            .call(operation, args)
            .map_err(|error| error.to_string());
        result
    }

    fn all_messages(&self, workspace_id: &str) -> Result<Vec<Value>, String> {
        let mut after = 0_u64;
        let mut messages = Vec::new();
        loop {
            let page = self.mail_read(
                workspace_id,
                "mail_history",
                json!({"after":after,"limit":200}),
            )?;
            let batch = page["messages"].as_array().cloned().unwrap_or_default();
            if batch.is_empty() {
                break;
            }
            after = batch
                .last()
                .and_then(|message| message["sequence"].as_u64())
                .ok_or_else(|| "mail history returned a message without sequence".to_owned())?;
            let count = batch.len();
            messages.extend(batch);
            if count < 200 {
                break;
            }
        }
        Ok(messages)
    }

    fn links_for(&self, workspace_id: &str, requested: &ResourceRef) -> Result<Value, String> {
        let mut outgoing = Vec::new();
        let mut incoming = Vec::new();
        let mut seen = HashSet::new();
        for message in self.all_messages(workspace_id)? {
            let message_id = match message["id"].as_str() {
                Some(id) => id,
                None => continue,
            };
            let message_ref = simple_ref(ResourceKind::Message, workspace_id, message_id);
            let refs = message["refs"].as_array().cloned().unwrap_or_default();
            let is_link_record = refs
                .iter()
                .any(|reference| reference["type"] == "orchard_resource_link");
            for value in refs {
                if value["type"] == "orchard_resource_link" {
                    let Some(source) = value.get("source").cloned() else {
                        continue;
                    };
                    let Some(target) = value.get("target").cloned() else {
                        continue;
                    };
                    let Ok(source) = serde_json::from_value::<ResourceRef>(source) else {
                        continue;
                    };
                    let Ok(target) = serde_json::from_value::<ResourceRef>(target) else {
                        continue;
                    };
                    if validate_ref(&source, workspace_id).is_err()
                        || validate_ref(&target, workspace_id).is_err()
                    {
                        continue;
                    }
                    append_link(
                        requested,
                        source,
                        target,
                        value["label"].as_str(),
                        &mut outgoing,
                        &mut incoming,
                        &mut seen,
                    )?;
                } else if !is_link_record {
                    if let Some(target) = normalize_attachment(&value, workspace_id) {
                        if validate_ref(&target, workspace_id).is_err() {
                            continue;
                        }
                        append_link(
                            requested,
                            message_ref.clone(),
                            target,
                            value["label"].as_str(),
                            &mut outgoing,
                            &mut incoming,
                            &mut seen,
                        )?;
                    }
                }
            }
        }
        Ok(json!({"outgoing":outgoing,"incoming":incoming}))
    }
}

fn parse_ref_arg(
    args: &serde_json::Map<String, Value>,
    key: &str,
    workspace_id: &str,
) -> Result<ResourceRef, String> {
    let mut reference: ResourceRef = serde_json::from_value(
        args.get(key)
            .cloned()
            .ok_or_else(|| format!("{key} is required"))?,
    )
    .map_err(|error| format!("invalid {key}: {error}"))?;
    if reference.workspace_id.is_empty() {
        reference.workspace_id = workspace_id.to_owned();
    }
    Ok(reference)
}

fn validate_ref(reference: &ResourceRef, workspace_id: &str) -> Result<(), String> {
    if reference.workspace_id.is_empty() || reference.workspace_id != workspace_id {
        return Err("cross-workspace resource references are not allowed".to_owned());
    }
    match reference.kind {
        ResourceKind::Channel
        | ResourceKind::Direct
        | ResourceKind::Message
        | ResourceKind::Agent => {
            let _ = required_ref(&reference.id, "id")?;
            reject_ref_fields(reference, false, true, true, true, true, true, true)?;
        }
        ResourceKind::Broadcast => {
            reject_ref_fields(reference, true, true, true, true, true, true, true)?;
        }
        ResourceKind::Task => {
            let _ = required_ref(&reference.store_id, "store_id")?;
            let _ = required_ref(&reference.task_id, "task_id")?;
            reject_ref_fields(reference, true, false, false, true, true, true, true)?;
        }
        ResourceKind::File => {
            let _ = required_ref(&reference.root_id, "root_id")?;
            validate_relative_path(required_ref(&reference.path, "path")?, false)?;
            if let Some(revision) = &reference.revision {
                parse_oid(revision)?;
            }
            reject_ref_fields(reference, true, true, true, false, false, false, true)?;
        }
        ResourceKind::Url => {
            validate_url(required_ref(&reference.url, "url")?)?;
            reject_ref_fields(reference, true, true, true, true, true, true, false)?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn reject_ref_fields(
    reference: &ResourceRef,
    id: bool,
    store_id: bool,
    task_id: bool,
    root_id: bool,
    path: bool,
    revision: bool,
    url: bool,
) -> Result<(), String> {
    let unexpected = (id && reference.id.is_some())
        || (store_id && reference.store_id.is_some())
        || (task_id && reference.task_id.is_some())
        || (root_id && reference.root_id.is_some())
        || (path && reference.path.is_some())
        || (revision && reference.revision.is_some())
        || (url && reference.url.is_some());
    if unexpected {
        return Err("resource reference contains fields that do not belong to its kind".to_owned());
    }
    Ok(())
}

fn required_ref<'a>(value: &'a Option<String>, name: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("resource {name} is required"))
}

pub(crate) fn format_href(reference: &ResourceRef) -> Result<String, String> {
    validate_ref(reference, &reference.workspace_id)?;
    let base = format!("/w/{}", percent_encode(&reference.workspace_id));
    Ok(match reference.kind {
        ResourceKind::Channel => format!(
            "{base}/channels/{}",
            percent_encode(required_ref(&reference.id, "id")?)
        ),
        ResourceKind::Direct => format!(
            "{base}/direct/{}",
            percent_encode(required_ref(&reference.id, "id")?)
        ),
        ResourceKind::Broadcast => format!("{base}/broadcast"),
        ResourceKind::Message => format!(
            "{base}/messages/{}",
            percent_encode(required_ref(&reference.id, "id")?)
        ),
        ResourceKind::Agent => format!(
            "{base}/agents/{}",
            percent_encode(required_ref(&reference.id, "id")?)
        ),
        ResourceKind::Task => format!(
            "{base}/tasks/{}/{}",
            percent_encode(required_ref(&reference.store_id, "store_id")?),
            percent_encode(required_ref(&reference.task_id, "task_id")?)
        ),
        ResourceKind::File => {
            let mut href = format!(
                "{base}/files/{}?path={}",
                percent_encode(required_ref(&reference.root_id, "root_id")?),
                percent_encode(required_ref(&reference.path, "path")?)
            );
            if let Some(revision) = &reference.revision {
                href.push_str("&revision=");
                href.push_str(revision);
            }
            href
        }
        ResourceKind::Url => format!(
            "{base}/urls?url={}",
            percent_encode(required_ref(&reference.url, "url")?)
        ),
    })
}

pub(crate) fn parse_href(href: &str) -> Result<ResourceRef, String> {
    let (path, query) = href.split_once('?').map_or((href, ""), |parts| parts);
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() < 4 || !parts[0].is_empty() || parts[1] != "w" {
        return Err("resource href is not canonical".to_owned());
    }
    let workspace_id = percent_decode(parts[2])?;
    let empty = || ResourceRef {
        kind: ResourceKind::Broadcast,
        workspace_id: workspace_id.clone(),
        id: None,
        store_id: None,
        task_id: None,
        root_id: None,
        path: None,
        revision: None,
        url: None,
    };
    let mut reference = empty();
    match (parts.get(3).copied(), parts.len()) {
        (Some("channels"), 5) => {
            reference.kind = ResourceKind::Channel;
            reference.id = Some(percent_decode(parts[4])?);
        }
        (Some("direct"), 5) => {
            reference.kind = ResourceKind::Direct;
            reference.id = Some(percent_decode(parts[4])?);
        }
        (Some("broadcast"), 4) => reference.kind = ResourceKind::Broadcast,
        (Some("messages"), 5) => {
            reference.kind = ResourceKind::Message;
            reference.id = Some(percent_decode(parts[4])?);
        }
        (Some("agents"), 5) => {
            reference.kind = ResourceKind::Agent;
            reference.id = Some(percent_decode(parts[4])?);
        }
        (Some("tasks"), 6) => {
            reference.kind = ResourceKind::Task;
            reference.store_id = Some(percent_decode(parts[4])?);
            reference.task_id = Some(percent_decode(parts[5])?);
        }
        (Some("files"), 5) => {
            reference.kind = ResourceKind::File;
            reference.root_id = Some(percent_decode(parts[4])?);
            let values = parse_query(query)?;
            reference.path = values.get("path").cloned();
            reference.revision = values.get("revision").cloned();
        }
        (Some("urls"), 4) => {
            reference.kind = ResourceKind::Url;
            reference.url = parse_query(query)?.get("url").cloned();
        }
        _ => return Err("resource href is not canonical".to_owned()),
    }
    let canonical = format_href(&reference)?;
    if canonical != href {
        return Err("resource href is not canonical".to_owned());
    }
    Ok(reference)
}

fn parse_query(query: &str) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    if query.is_empty() {
        return Ok(values);
    }
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| "resource query is malformed".to_owned())?;
        let key = percent_decode(key)?;
        let value = percent_decode(value)?;
        if values.insert(key, value).is_some() {
            return Err("duplicate resource query field".to_owned());
        }
    }
    Ok(values)
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err("invalid percent encoding".to_owned());
            }
            let high = hex(bytes[index + 1])?;
            let low = hex(bytes[index + 2])?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| "resource href is not UTF-8".to_owned())
}

fn hex(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("invalid percent encoding".to_owned()),
    }
}

fn kind_name(kind: &ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Channel => "channel",
        ResourceKind::Direct => "direct",
        ResourceKind::Broadcast => "broadcast",
        ResourceKind::Message => "message",
        ResourceKind::Agent => "agent",
        ResourceKind::Task => "task",
        ResourceKind::File => "file",
        ResourceKind::Url => "url",
    }
}

fn simple_ref(kind: ResourceKind, workspace_id: &str, id: &str) -> ResourceRef {
    ResourceRef {
        kind,
        workspace_id: workspace_id.to_owned(),
        id: Some(id.to_owned()),
        store_id: None,
        task_id: None,
        root_id: None,
        path: None,
        revision: None,
        url: None,
    }
}

fn normalize_attachment(value: &Value, workspace_id: &str) -> Option<ResourceRef> {
    if value["type"] == "resource" {
        return serde_json::from_value(value.get("resource")?.clone()).ok();
    }
    if let Some(task) = value.get("task_ref") {
        return Some(ResourceRef {
            kind: ResourceKind::Task,
            workspace_id: workspace_id.to_owned(),
            id: None,
            store_id: task["store_id"].as_str().map(str::to_owned),
            task_id: task["task_id"].as_str().map(str::to_owned),
            root_id: None,
            path: None,
            revision: None,
            url: None,
        });
    }
    if let Some(url) = value.get("url").and_then(Value::as_str) {
        return Some(ResourceRef {
            kind: ResourceKind::Url,
            workspace_id: workspace_id.to_owned(),
            id: None,
            store_id: None,
            task_id: None,
            root_id: None,
            path: None,
            revision: None,
            url: Some(url.to_owned()),
        });
    }
    if let (Some(root_id), Some(path)) = (
        value.get("root_id").and_then(Value::as_str),
        value.get("path").and_then(Value::as_str),
    ) {
        return Some(ResourceRef {
            kind: ResourceKind::File,
            workspace_id: workspace_id.to_owned(),
            id: None,
            store_id: None,
            task_id: None,
            root_id: Some(root_id.to_owned()),
            path: Some(path.to_owned()),
            revision: value
                .get("revision")
                .and_then(Value::as_str)
                .map(str::to_owned),
            url: None,
        });
    }
    None
}

fn append_link(
    requested: &ResourceRef,
    source: ResourceRef,
    target: ResourceRef,
    label: Option<&str>,
    outgoing: &mut Vec<Value>,
    incoming: &mut Vec<Value>,
    seen: &mut HashSet<String>,
) -> Result<(), String> {
    let requested_href = format_href(requested)?;
    let source_href = format_href(&source)?;
    let target_href = format_href(&target)?;
    let is_outgoing = source_href == requested_href;
    let is_incoming = target_href == requested_href;
    if !is_outgoing && !is_incoming {
        return Ok(());
    }
    let key = format!("{source_href}\0{target_href}");
    if !seen.insert(key) {
        return Ok(());
    }
    let link = json!({"source":source,"target":target,"label":label,"href":target_href});
    if is_outgoing {
        outgoing.push(link.clone());
    }
    if is_incoming {
        incoming.push(link);
    }
    Ok(())
}

fn artifact_roots_for(workspace: &WorkspaceConfig) -> Vec<ArtifactRoot> {
    let mut roots = vec![ArtifactRoot {
        id: "artifacts".to_owned(),
        name: "Artifacts".to_owned(),
        path: workspace.root.join("artifacts"),
        owned: true,
    }];
    roots.extend(
        workspace
            .repositories
            .iter()
            .map(|repository| ArtifactRoot {
                id: repository.id.clone(),
                name: if repository.name.is_empty() {
                    repository
                        .path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("Repository")
                        .to_owned()
                } else {
                    repository.name.clone()
                },
                path: repository.path.clone(),
                owned: false,
            }),
    );
    roots
}

fn find_root(workspace: &WorkspaceConfig, root_id: &str) -> Result<ArtifactRoot, String> {
    artifact_roots_for(workspace)
        .into_iter()
        .find(|root| root.id == root_id)
        .ok_or_else(|| format!("unknown artifact root {root_id:?}"))
}

fn open_root_repo(root: &ArtifactRoot) -> Result<Repository, String> {
    if !root.path.exists() {
        return Err(format!("artifact root {} is missing", root.path.display()));
    }
    let metadata = fs::symlink_metadata(&root.path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("artifact root cannot be a symlink".to_owned());
    }
    Repository::open(&root.path)
        .map_err(|error| format!("artifact root is not a Git repository: {error}"))
}

fn create_owned_root(workspace_root: &Path, artifact_root: &Path) -> Result<(), String> {
    let workspace_metadata =
        fs::symlink_metadata(workspace_root).map_err(|error| error.to_string())?;
    if workspace_metadata.file_type().is_symlink() || !workspace_metadata.is_dir() {
        return Err("workspace root is not a safe directory".to_owned());
    }
    if artifact_root.exists() {
        let metadata = fs::symlink_metadata(artifact_root).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("owned artifact root is not a safe directory".to_owned());
        }
    } else {
        fs::create_dir(artifact_root).map_err(|error| error.to_string())?;
    }
    let workspace = workspace_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let artifact = artifact_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if !artifact.starts_with(&workspace) {
        return Err("owned artifact root escapes the workspace".to_owned());
    }
    Ok(())
}

fn validate_owned_git_metadata(artifact_root: &Path, git_dir: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(git_dir).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("owned artifact .git must be a local directory".to_owned());
    }
    let root = artifact_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let git_dir = git_dir.canonicalize().map_err(|error| error.to_string())?;
    if !git_dir.starts_with(&root) {
        return Err("owned artifact Git metadata escapes its root".to_owned());
    }
    Ok(())
}

fn validate_owned_repository(artifact_root: &Path, repo: &Repository) -> Result<(), String> {
    if repo.is_bare() {
        return Err("owned artifact repository must have a working tree".to_owned());
    }
    let root = artifact_root
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| "owned artifact repository has no working tree".to_owned())?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let git_dir = repo
        .path()
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if workdir != root || !git_dir.starts_with(&root) {
        return Err("owned artifact repository points outside its root".to_owned());
    }
    Ok(())
}

fn ensure_safe_live_path(root: &Path, relative: &str, must_exist: bool) -> Result<(), String> {
    let root_metadata = fs::symlink_metadata(root).map_err(|error| error.to_string())?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("artifact root is not a safe directory".to_owned());
    }
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut current = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Err("artifact path must be a normalized relative path".to_owned());
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("artifact path contains a symlink".to_owned());
                }
                let canonical = current.canonicalize().map_err(|error| error.to_string())?;
                if !canonical.starts_with(&canonical_root) {
                    return Err("artifact path escapes its root".to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !must_exist => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    if must_exist && !current.exists() {
        return Err("artifact is missing".to_owned());
    }
    Ok(())
}

fn validate_relative_path(path: &str, allow_empty: bool) -> Result<(), String> {
    if path.is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err("artifact path must not be empty".to_owned())
        };
    }
    if path.contains('\\') || Path::new(path).is_absolute() {
        return Err("artifact path must be a normalized relative path".to_owned());
    }
    let mut normalized = Vec::new();
    for component in Path::new(path).components() {
        let Component::Normal(value) = component else {
            return Err("artifact path must be a normalized relative path".to_owned());
        };
        let value = value
            .to_str()
            .ok_or_else(|| "artifact path is not UTF-8".to_owned())?;
        if value.eq_ignore_ascii_case(".git")
            || value.eq_ignore_ascii_case(".orchard")
            || value.eq_ignore_ascii_case("credentials")
        {
            return Err("artifact path enters a protected directory".to_owned());
        }
        normalized.push(value);
    }
    if normalized.join("/") != path {
        return Err("artifact path must be a normalized relative path".to_owned());
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<(), String> {
    let authority = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    if authority.is_none_or(|value| {
        value.is_empty()
            || value.starts_with('/')
            || value
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
    }) {
        return Err("resource URL must be HTTP or HTTPS".to_owned());
    }
    Ok(())
}

fn parse_oid(revision: &str) -> Result<Oid, String> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("revision must be a full 40-character Git object id".to_owned());
    }
    Oid::from_str(revision).map_err(git_error)
}

fn list_live_index(
    repo: &Repository,
    path: &str,
) -> Result<Vec<(String, String, &'static str)>, String> {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    let mut entries = BTreeMap::<String, (String, &'static str)>::new();
    for entry in repo.index().map_err(git_error)?.iter() {
        let full = std::str::from_utf8(&entry.path)
            .map_err(|_| "Git index path is not UTF-8".to_owned())?;
        if !full.starts_with(&prefix) {
            continue;
        }
        let remainder = &full[prefix.len()..];
        let Some(first) = remainder.split('/').next() else {
            continue;
        };
        let child = if path.is_empty() {
            first.to_owned()
        } else {
            format!("{path}/{first}")
        };
        if validate_relative_path(&child, false).is_err() {
            continue;
        }
        let kind = if remainder.contains('/') {
            "directory"
        } else if entry.mode == 0o120000 {
            "symlink"
        } else if entry.mode == 0o160000 {
            "submodule"
        } else {
            "file"
        };
        entries.entry(first.to_owned()).or_insert((child, kind));
    }
    Ok(entries
        .into_iter()
        .map(|(name, (path, kind))| (name, path, kind))
        .collect())
}

fn list_tree(
    repo: &Repository,
    revision: Oid,
    path: &str,
) -> Result<Vec<(String, String, &'static str)>, String> {
    let commit = repo.find_commit(revision).map_err(git_error)?;
    let root = commit.tree().map_err(git_error)?;
    let tree = if path.is_empty() {
        root
    } else {
        let entry = root.get_path(Path::new(path)).map_err(git_error)?;
        repo.find_tree(entry.id()).map_err(git_error)?
    };
    let mut entries = Vec::new();
    for entry in &tree {
        let Some(name) = entry.name() else { continue };
        let full = if path.is_empty() {
            name.to_owned()
        } else {
            format!("{path}/{name}")
        };
        if validate_relative_path(&full, false).is_err() {
            continue;
        }
        let kind = match (entry.kind(), entry.filemode()) {
            (_, 0o120000) => "symlink",
            (Some(ObjectType::Commit), _) => "submodule",
            (Some(ObjectType::Tree), _) => "directory",
            (Some(ObjectType::Blob), _) => "file",
            _ => continue,
        };
        entries.push((name.to_owned(), full, kind));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

fn tree_entry_identity(tree: &Tree<'_>, path: &str) -> Option<(Oid, i32)> {
    tree.get_path(Path::new(path))
        .ok()
        .map(|entry| (entry.id(), entry.filemode()))
}

fn commit_paths(repo: &Repository, paths: &[&str], request_id: &str) -> Result<Oid, String> {
    let mut index = repo.index().map_err(git_error)?;
    for path in paths {
        index.add_path(Path::new(path)).map_err(git_error)?;
    }
    index.write().map_err(git_error)?;
    let tree_id = index.write_tree().map_err(git_error)?;
    let tree = repo.find_tree(tree_id).map_err(git_error)?;
    let signature = Signature::now("Orchard", "orchard@localhost").map_err(git_error)?;
    let parent = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .and_then(|oid| repo.find_commit(oid).ok());
    let parents = parent.iter().collect::<Vec<&Commit<'_>>>();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        &format!("Orchard upload {request_id}"),
        &tree,
        &parents,
    )
    .map_err(git_error)
}

fn find_upload_commit(
    repo: &Repository,
    receipt_path: &str,
    path: &str,
    fingerprint: &str,
) -> Result<Option<Oid>, String> {
    let Some(mut commit) = repo
        .head()
        .ok()
        .and_then(|head| head.target())
        .and_then(|oid| repo.find_commit(oid).ok())
    else {
        return Ok(None);
    };
    let mut original = None;
    loop {
        let tree = commit.tree().map_err(git_error)?;
        match tree.get_path(Path::new(receipt_path)) {
            Ok(receipt_entry) => {
                let receipt = repo.find_blob(receipt_entry.id()).map_err(git_error)?;
                let receipt = serde_json::from_slice::<UploadReceipt>(receipt.content())
                    .map_err(|_| "stored upload receipt is invalid".to_owned())?;
                if receipt.path != path || receipt.fingerprint != fingerprint {
                    return Err("request_id was already used for a different upload".to_owned());
                }
                original = Some(commit.id());
            }
            Err(_) if original.is_some() => break,
            Err(_) => {}
        }
        let Ok(parent) = commit.parent(0) else { break };
        commit = parent;
    }
    if let Some(revision) = original {
        let commit = repo.find_commit(revision).map_err(git_error)?;
        let tree = commit.tree().map_err(git_error)?;
        let file_entry = tree
            .get_path(Path::new(path))
            .map_err(|_| "uploaded artifact is missing from its original commit".to_owned())?;
        let file = repo.find_blob(file_entry.id()).map_err(git_error)?;
        if format!("{:x}", Sha256::digest(file.content())) != fingerprint {
            return Err("uploaded artifact does not match its receipt".to_owned());
        }
    }
    Ok(original)
}

fn read_pending_receipt(
    receipt_file: &Path,
    path: &str,
    fingerprint: &str,
) -> Result<bool, String> {
    let bytes = match fs::read(receipt_file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    let receipt = serde_json::from_slice::<UploadReceipt>(&bytes)
        .map_err(|_| "pending upload receipt is invalid".to_owned())?;
    if receipt.path != path || receipt.fingerprint != fingerprint {
        return Err("request_id was already used for a different upload".to_owned());
    }
    Ok(true)
}

fn validate_request_id(request_id: &str) -> Result<(), String> {
    if request_id.is_empty()
        || request_id.len() > 128
        || !request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(
            "request_id must contain only letters, numbers, dot, dash, or underscore".to_owned(),
        );
    }
    Ok(())
}

fn download_href(workspace_id: &str, root_id: &str, path: &str, revision: Option<&str>) -> String {
    let mut href = format!(
        "/api/workspaces/{}/artifact/download?root_id={}&path={}",
        percent_encode(workspace_id),
        percent_encode(root_id),
        percent_encode(path)
    );
    if let Some(revision) = revision {
        href.push_str("&revision=");
        href.push_str(revision);
    }
    href
}

fn preview_href(workspace_id: &str, root_id: &str, path: &str, revision: Option<&str>) -> String {
    let mut href = download_href(workspace_id, root_id, path, revision);
    href.push_str("&preview=true");
    href
}

pub(crate) fn percent_encode_header_value(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn safe_preview_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn git_error(error: git2::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn canonical_resource_hrefs_round_trip_every_kind() {
        let workspace = "space one";
        let refs = vec![
            simple_ref(ResourceKind::Channel, workspace, "general/a"),
            simple_ref(ResourceKind::Direct, workspace, "agent one"),
            ResourceRef {
                kind: ResourceKind::Broadcast,
                workspace_id: workspace.to_owned(),
                id: None,
                store_id: None,
                task_id: None,
                root_id: None,
                path: None,
                revision: None,
                url: None,
            },
            simple_ref(ResourceKind::Message, workspace, "m_1"),
            simple_ref(ResourceKind::Agent, workspace, "agent one"),
            ResourceRef {
                kind: ResourceKind::Task,
                workspace_id: workspace.to_owned(),
                id: None,
                store_id: Some("store/a".to_owned()),
                task_id: Some("task 1".to_owned()),
                root_id: None,
                path: None,
                revision: None,
                url: None,
            },
            ResourceRef {
                kind: ResourceKind::File,
                workspace_id: workspace.to_owned(),
                id: None,
                store_id: None,
                task_id: None,
                root_id: Some("repo 1".to_owned()),
                path: Some("docs/a.b.md".to_owned()),
                revision: Some("a".repeat(40)),
                url: None,
            },
            ResourceRef {
                kind: ResourceKind::Url,
                workspace_id: workspace.to_owned(),
                id: None,
                store_id: None,
                task_id: None,
                root_id: None,
                path: None,
                revision: None,
                url: Some("https://example.com/a?q=one%20two".to_owned()),
            },
        ];
        for reference in refs {
            let href = format_href(&reference).unwrap();
            assert_eq!(parse_href(&href).unwrap(), reference);
        }
    }

    #[test]
    fn rejects_noncanonical_and_unsafe_paths() {
        assert!(parse_href("/w/a/files/b?path=../secret").is_err());
        assert!(parse_href("/w/a/files/b?path=docs%2F.%2Fsecret").is_err());
        assert!(validate_relative_path("docs/.git/config", false).is_err());
        assert!(validate_relative_path("docs/.Git/config", false).is_err());
        assert!(validate_relative_path("docs/.ORCHARD/receipt", false).is_err());
        assert!(validate_relative_path("docs//file", false).is_err());
        assert!(validate_relative_path("docs/./file", false).is_err());
        assert!(validate_relative_path("docs/file/", false).is_err());
        assert!(validate_relative_path("docs/100%.md", false).is_ok());
        assert!(parse_oid("abc").is_err());
        let mut channel = simple_ref(ResourceKind::Channel, "workspace", "general");
        channel.root_id = Some("artifacts".to_owned());
        assert!(validate_ref(&channel, "workspace")
            .unwrap_err()
            .contains("do not belong"));
    }

    #[test]
    fn href_encoding_matches_strict_rfc3986_golden() {
        let reference = ResourceRef {
            kind: ResourceKind::File,
            workspace_id: "work !'()*%雪".to_owned(),
            id: None,
            store_id: None,
            task_id: None,
            root_id: Some("repo!".to_owned()),
            path: Some("docs/it's (100%) 雪.md".to_owned()),
            revision: None,
            url: None,
        };
        let href = "/w/work%20%21%27%28%29%2A%25%E9%9B%AA/files/repo%21?path=docs%2Fit%27s%20%28100%25%29%20%E9%9B%AA.md";
        assert_eq!(format_href(&reference).unwrap(), href);
        assert_eq!(parse_href(href).unwrap(), reference);
        assert_eq!(
            percent_encode_header_value("space ü'().txt"),
            "space%20%C3%BC%27%28%29.txt"
        );
    }

    #[test]
    fn upload_receipt_replays_original_commit_and_conflicts_on_reuse() {
        let temp = TempDir::new().unwrap();
        let repo = Repository::init(temp.path()).unwrap();
        fs::create_dir_all(temp.path().join(".orchard/requests")).unwrap();
        fs::write(temp.path().join("first.txt"), b"first").unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(b"first"));
        let receipt = UploadReceipt {
            path: "first.txt".to_owned(),
            fingerprint: fingerprint.clone(),
        };
        fs::write(
            temp.path().join(".orchard/requests/request-one.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        let first = commit_paths(
            &repo,
            &["first.txt", ".orchard/requests/request-one.json"],
            "request-one",
        )
        .unwrap();
        fs::write(temp.path().join("second.txt"), b"second").unwrap();
        let second_fingerprint = format!("{:x}", Sha256::digest(b"second"));
        fs::write(
            temp.path().join(".orchard/requests/request-two.json"),
            serde_json::to_vec(&UploadReceipt {
                path: "second.txt".to_owned(),
                fingerprint: second_fingerprint,
            })
            .unwrap(),
        )
        .unwrap();
        let second = commit_paths(
            &repo,
            &["second.txt", ".orchard/requests/request-two.json"],
            "request-two",
        )
        .unwrap();
        assert_ne!(first, second);
        assert_eq!(
            find_upload_commit(
                &repo,
                ".orchard/requests/request-one.json",
                "first.txt",
                &fingerprint
            )
            .unwrap(),
            Some(first)
        );
        assert!(find_upload_commit(
            &repo,
            ".orchard/requests/request-one.json",
            "other.txt",
            &fingerprint
        )
        .unwrap_err()
        .contains("different upload"));
    }

    #[cfg(unix)]
    #[test]
    fn live_path_rejects_symlink_ancestors() {
        use std::os::unix::fs::symlink;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, root.join("docs")).unwrap();
        assert!(ensure_safe_live_path(&root, "docs/escape.txt", false)
            .unwrap_err()
            .contains("symlink"));
    }
}
