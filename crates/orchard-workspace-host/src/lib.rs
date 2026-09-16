mod beads;
mod config;
mod mcp;

use axum::extract::{DefaultBodyLimit, Json, Path as AxumPath, State};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::{body::Body, Router};
use beads::{BeadsAdapter, CommandFailure, CreateTask};
use config::{AppConfig, RepositoryConfig, TaskStoreConfig, WorkspaceConfig};
use fs2::FileExt;
use git2::Repository;
use orchard_mail_core::MailService;
use orchard_mail_mcp::{build_router_with_cancellation, CoreBackend};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock, Weak};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

const MAIL_OPERATIONS: &[&str] = &[
    "mail_register",
    "mail_resume",
    "mail_leave",
    "mail_participants",
    "mail_channel_create",
    "mail_channels",
    "mail_send",
    "mail_inbox",
    "mail_history",
    "mail_acknowledge",
    "mail_search",
];

#[derive(Debug, Error)]
pub enum HostError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid persisted JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported Orchard config version {0}")]
    UnsupportedConfigVersion(u32),
    #[error("the Orchard data root is already owned by another process")]
    AlreadyRunning,
    #[error("Beads backend unavailable: {0}")]
    Beads(String),
    #[error("mail backend unavailable: {0}")]
    Mail(String),
    #[error("server error: {0}")]
    Server(String),
}

pub struct WorkspaceHost {
    inner: Arc<HostInner>,
}

type RequestKey = (String, String);
type RequestLock = Arc<Mutex<()>>;

pub(crate) struct HostInner {
    data_root: PathBuf,
    _data_lock: File,
    beads: BeadsAdapter,
    config: Mutex<AppConfig>,
    runtimes: RwLock<HashMap<String, Arc<WorkspaceRuntime>>>,
    store_locks: RwLock<HashMap<PathBuf, Arc<Mutex<()>>>>,
    request_locks: Mutex<HashMap<RequestKey, RequestLock>>,
    endpoint: Mutex<Option<SocketAddr>>,
    owner_token: String,
    owner_token_path: PathBuf,
    browser_sessions: Mutex<HashMap<String, ()>>,
}

struct WorkspaceRuntime {
    id: String,
    token: RwLock<String>,
    mail: Option<Arc<Mutex<MailService>>>,
    mail_error: Option<String>,
    mcp_router: RwLock<Option<Router>>,
    mcp_cancellation: Mutex<CancellationToken>,
}

#[derive(Clone, Debug)]
struct TaskReceipt {
    kind: String,
    fingerprint: String,
    outcome: String,
    task_id: Option<String>,
    error: Option<String>,
}

pub struct ServerHandle {
    endpoint: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), std::io::Error>>,
    host: Weak<HostInner>,
}

#[derive(Clone, Debug)]
pub struct OwnerBootstrap {
    pub endpoint: SocketAddr,
    pub credential_path: PathBuf,
    pub token: String,
}

impl ServerHandle {
    pub fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }

    pub async fn shutdown(mut self) -> Result<(), HostError> {
        let host = self.host.upgrade();
        if let Some(host) = &host {
            for runtime in host.runtimes.read().unwrap().values() {
                runtime.mcp_cancellation.lock().unwrap().cancel();
            }
        }
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        let result = match timeout(Duration::from_secs(3), &mut self.task).await {
            Ok(result) => result
                .map_err(|error| HostError::Server(error.to_string()))?
                .map_err(HostError::Io),
            Err(_) => {
                self.task.abort();
                let _ = self.task.await;
                Err(HostError::Server(
                    "server did not drain within three seconds and was aborted".to_owned(),
                ))
            }
        };
        if let Some(host) = host {
            *host.endpoint.lock().unwrap() = None;
            host.browser_sessions.lock().unwrap().clear();
        }
        result
    }
}

impl WorkspaceHost {
    pub fn open(data_root: PathBuf, br_path: PathBuf) -> Result<Self, HostError> {
        Self::open_with_port(data_root, br_path, None)
    }

    pub fn open_with_port(
        data_root: PathBuf,
        br_path: PathBuf,
        port: Option<u16>,
    ) -> Result<Self, HostError> {
        if port == Some(0) {
            return Err(HostError::Server(
                "explicit port must be between 1 and 65535".to_owned(),
            ));
        }
        create_private_dir(&data_root)?;
        let lock_path = data_root.join("host.lock");
        let data_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        data_lock
            .try_lock_exclusive()
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::WouldBlock => HostError::AlreadyRunning,
                _ => HostError::Io(error),
            })?;

        let beads = BeadsAdapter::open(br_path);
        let mut config = config::load(&data_root)?;
        if let Some(port) = port {
            if config.port != Some(port) {
                config.port = Some(port);
                config::save(&data_root, &config)?;
            }
        }
        create_private_dir(&data_root.join("credentials"))?;
        let (owner_token_path, owner_token) = read_or_create_owner_token(&data_root)?;

        let mut runtimes = HashMap::new();
        let mut store_locks = HashMap::new();
        for workspace in config
            .workspaces
            .iter()
            .filter(|workspace| !workspace.archived)
        {
            let runtime = load_runtime(&data_root, workspace)?;
            runtimes.insert(workspace.id.clone(), Arc::new(runtime));
            for store in &workspace.task_stores {
                store_locks
                    .entry(store.db_path.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(())));
            }
        }

        Ok(Self {
            inner: Arc::new(HostInner {
                data_root,
                _data_lock: data_lock,
                beads,
                config: Mutex::new(config),
                runtimes: RwLock::new(runtimes),
                store_locks: RwLock::new(store_locks),
                request_locks: Mutex::new(HashMap::new()),
                endpoint: Mutex::new(None),
                owner_token,
                owner_token_path,
                browser_sessions: Mutex::new(HashMap::new()),
            }),
        })
    }

    pub fn call(&self, operation: &str, args: Value) -> Result<Value, String> {
        match operation {
            "workspace_list" => self.workspace_list(),
            "workspace_create" => self.workspace_create(args),
            "workspace_archive" => self.workspace_archive(args),
            "workspace_snapshot" => self.workspace_snapshot(args),
            "workspace_info" => self.workspace_info(args),
            "connection_info" => self.connection_info(args),
            "rotate_token" => self.rotate_token(args),
            "repository_attach" => self.repository_attach(args),
            "repository_detach" => self.repository_detach(args),
            "task_store_attach" => self.task_store_attach(args),
            "task_store_detach" => self.task_store_detach(args),
            "tasks_list" => self.tasks_list(args),
            "task_show" => self.task_show(args),
            "task_create" => self.task_create(args),
            "task_update" => self.task_update(args),
            "task_close" => self.task_close(args),
            "task_dependencies" => self.task_dependencies(args),
            "settings_get" => self.settings_get(),
            operation if MAIL_OPERATIONS.contains(&operation) => self.mail_call(operation, args),
            _ => Err(format!("unsupported workspace operation {operation:?}")),
        }
    }

    pub async fn start_server(self: Arc<Self>) -> Result<ServerHandle, HostError> {
        self.start_server_with_ui(Router::new()).await
    }

    pub async fn start_server_with_ui(
        self: Arc<Self>,
        ui: Router,
    ) -> Result<ServerHandle, HostError> {
        if self.inner.endpoint.lock().unwrap().is_some() {
            return Err(HostError::Server("server is already running".to_owned()));
        }

        self.rebuild_all_mcp_routers();
        let saved_port = self.inner.config.lock().unwrap().port;
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), saved_port.unwrap_or(0));
        let listener = TcpListener::bind(address).await.map_err(|error| {
            if let Some(saved_port) = saved_port {
                HostError::Server(format!(
                    "persisted loopback port {} is unavailable; refusing to move the endpoint: {error}",
                    saved_port
                ))
            } else {
                HostError::Io(error)
            }
        })?;
        let endpoint = listener.local_addr()?;
        if saved_port.is_none() {
            let mut config = self.inner.config.lock().unwrap();
            config.port = Some(endpoint.port());
            config::save(&self.inner.data_root, &config)?;
        }
        *self.inner.endpoint.lock().unwrap() = Some(endpoint);

        let api = Router::new()
            .route("/workspaces/{workspace_id}/mcp", any(dynamic_mcp))
            .route(
                "/api/session",
                axum::routing::get(api_session_get)
                    .post(api_session_post)
                    .delete(api_session_delete),
            )
            .route("/api/call", axum::routing::post(api_call))
            .layer(DefaultBodyLimit::max(1024 * 1024))
            .with_state(self.clone());
        let app = api.merge(ui);
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = receiver.await;
                })
                .await
        });
        Ok(ServerHandle {
            endpoint,
            shutdown: Some(shutdown),
            task,
            host: Arc::downgrade(&self.inner),
        })
    }

    pub fn owner_bootstrap(&self) -> Result<OwnerBootstrap, HostError> {
        let endpoint = self
            .inner
            .endpoint
            .lock()
            .unwrap()
            .ok_or_else(|| HostError::Server("server has not started".to_owned()))?;
        Ok(OwnerBootstrap {
            endpoint,
            credential_path: self.inner.owner_token_path.clone(),
            token: self.inner.owner_token.clone(),
        })
    }

    fn workspace_list(&self) -> Result<Value, String> {
        let config = self.inner.config.lock().unwrap();
        Ok(json!({
            "workspaces": config.workspaces.iter().map(workspace_view).collect::<Vec<_>>()
        }))
    }

    fn workspace_create(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let name = required_string(&args, "name")?;
        let owner_name =
            optional_string(&args, "owner_name")?.unwrap_or_else(|| "Owner".to_owned());
        if name.trim().is_empty() {
            return Err("workspace name must not be empty".to_owned());
        }
        let id = Uuid::new_v4().simple().to_string();
        let requested_root = optional_string(&args, "root")?.map(PathBuf::from);
        let root =
            requested_root.unwrap_or_else(|| self.inner.data_root.join("workspaces").join(&id));
        create_private_dir(&root).map_err(|error| error.to_string())?;
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let mail_path = root.join("mail");
        let mail = MailService::open(&mail_path).map_err(|error| error.to_string())?;
        let token = new_token();
        write_token(&self.inner.data_root, &id, &token).map_err(|error| error.to_string())?;

        let mut workspace = WorkspaceConfig {
            id: id.clone(),
            name,
            root,
            mail_path,
            archived: false,
            repositories: Vec::new(),
            task_stores: Vec::new(),
        };
        if self.inner.beads.availability().is_ok() {
            let task_root = workspace.root.join("tasks");
            create_private_dir(&task_root).map_err(|error| error.to_string())?;
            let mut store = self
                .inner
                .beads
                .init_owned_store(&task_root, "orchard")
                .map_err(|error| {
                    format!("workspace mail was created, but its owned task store failed: {error}")
                })?;
            store.id = "default".to_owned();
            workspace.task_stores.push(store);
        }
        let runtime = Arc::new(WorkspaceRuntime {
            id: id.clone(),
            token: RwLock::new(token),
            mail: Some(Arc::new(Mutex::new(mail))),
            mail_error: None,
            mcp_router: RwLock::new(None),
            mcp_cancellation: Mutex::new(CancellationToken::new()),
        });
        self.initialize_workspace_actors(&runtime, &owner_name)?;

        {
            let mut config = self.inner.config.lock().unwrap();
            config.workspaces.push(workspace.clone());
            config::save(&self.inner.data_root, &config).map_err(|error| error.to_string())?;
        }
        self.inner
            .runtimes
            .write()
            .unwrap()
            .insert(id.clone(), runtime.clone());
        for store in &workspace.task_stores {
            self.inner
                .store_locks
                .write()
                .unwrap()
                .entry(store.db_path.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())));
        }
        self.rebuild_mcp_router(&runtime);
        Ok(json!({
            "workspace": workspace_view(&workspace),
            "task_backend": task_backend_view(&self.inner.beads)
        }))
    }

    fn workspace_archive(&self, args: Value) -> Result<Value, String> {
        let workspace_id = workspace_id(&args)?;
        let workspace = {
            let mut config = self.inner.config.lock().unwrap();
            let workspace = config
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.id == workspace_id)
                .ok_or_else(|| format!("unknown workspace {workspace_id:?}"))?;
            workspace.archived = true;
            let snapshot = workspace.clone();
            config::save(&self.inner.data_root, &config).map_err(|error| error.to_string())?;
            snapshot
        };
        if let Some(runtime) = self.inner.runtimes.write().unwrap().remove(&workspace_id) {
            runtime.mcp_cancellation.lock().unwrap().cancel();
        }
        Ok(json!({"workspace": workspace_view(&workspace)}))
    }

    fn connection_info(&self, args: Value) -> Result<Value, String> {
        let workspace_id = workspace_id(&args)?;
        let runtime = self.active_runtime(&workspace_id)?;
        let endpoint = self
            .inner
            .endpoint
            .lock()
            .unwrap()
            .ok_or_else(|| "MCP server has not started".to_owned())?;
        Ok(json!({
            "workspace_id": workspace_id,
            "endpoint": format!("http://{endpoint}/workspaces/{}/mcp", runtime.id),
            "token": runtime.token.read().unwrap().clone()
        }))
    }

    fn rotate_token(&self, args: Value) -> Result<Value, String> {
        let workspace_id = workspace_id(&args)?;
        let runtime = self.active_runtime(&workspace_id)?;
        let token = new_token();
        write_token(&self.inner.data_root, &workspace_id, &token)
            .map_err(|error| error.to_string())?;
        *runtime.token.write().unwrap() = token.clone();
        self.rebuild_mcp_router(&runtime);
        Ok(json!({"workspace_id": workspace_id, "token": token, "rotated": true}))
    }

    fn repository_attach(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        self.active_runtime(&workspace_id)?;
        let requested = PathBuf::from(required_string(&args, "path")?);
        let repository = Repository::discover(&requested).map_err(|error| {
            format!(
                "{} is not inside a Git repository: {error}",
                requested.display()
            )
        })?;
        let path = repository
            .workdir()
            .unwrap_or_else(|| repository.path())
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let name = repository_name(&path);
        let discovered = inspect_repository_task_store(&path);
        let mut config = self.inner.config.lock().unwrap();
        let mut next_config = config.clone();
        let matching_store = discovered.store.as_ref().and_then(|candidate| {
            next_config.workspaces.iter().find_map(|workspace| {
                workspace
                    .task_stores
                    .iter()
                    .find(|store| store.db_path == candidate.db_path)
                    .map(|store| (workspace.id.clone(), store.clone()))
            })
        });
        if let Some((owner, _)) = &matching_store {
            if owner != &workspace_id {
                return Err(format!(
                    "task store {} is already attached to workspace {}; a canonical database has one Orchard lock owner",
                    discovered.store.as_ref().unwrap().db_path.display(), owner
                ));
            }
        }

        let workspace = active_workspace_mut(&mut next_config, &workspace_id)?;
        let existing_index = workspace
            .repositories
            .iter()
            .position(|repository| repository.path == path);
        let attached = existing_index.is_none();
        let repository_id = existing_index
            .map(|index| workspace.repositories[index].id.clone())
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let mut task_store_attached = false;
        let task_store_id = if let Some((_, store)) = matching_store {
            Some(store.id)
        } else if let Some(mut store) = discovered.store.clone() {
            store.id = Uuid::new_v4().simple().to_string();
            store.source = Some("repository".to_owned());
            store.repository_id = Some(repository_id.clone());
            let id = store.id.clone();
            workspace.task_stores.push(store);
            task_store_attached = true;
            Some(id)
        } else {
            existing_index.and_then(|index| workspace.repositories[index].task_store_id.clone())
        };
        let repository = RepositoryConfig {
            id: repository_id,
            path,
            name,
            task_store_id,
            task_status: discovered.status,
            task_error: discovered.error,
        };
        if let Some(index) = existing_index {
            let replaced_store_id = workspace.repositories[index]
                .task_store_id
                .as_ref()
                .filter(|store_id| Some(*store_id) != repository.task_store_id.as_ref())
                .cloned();
            workspace.repositories[index] = repository.clone();
            if let Some(store_id) = replaced_store_id {
                let still_referenced = workspace
                    .repositories
                    .iter()
                    .any(|repository| repository.task_store_id.as_deref() == Some(&store_id));
                if !still_referenced {
                    if let Some(store) = workspace.task_stores.iter_mut().find(|store| {
                        store.id == store_id
                            && store.repository_id.as_deref() == Some(&repository.id)
                    }) {
                        store.repository_id = None;
                        store.source = Some("external".to_owned());
                    }
                }
            }
        } else {
            workspace.repositories.push(repository.clone());
        }
        let task_store = repository.task_store_id.as_ref().and_then(|store_id| {
            workspace
                .task_stores
                .iter()
                .find(|store| &store.id == store_id)
                .cloned()
        });
        config::save(&self.inner.data_root, &next_config).map_err(|error| error.to_string())?;
        *config = next_config;
        if let Some(store) = &task_store {
            self.inner
                .store_locks
                .write()
                .unwrap()
                .entry(store.db_path.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())));
        }
        Ok(json!({
            "repository": repository_view(&repository, task_store.as_ref()),
            "task_store": task_store.as_ref().map(task_store_view),
            "attached": attached,
            "task_store_attached": task_store_attached
        }))
    }

    fn repository_detach(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        self.active_runtime(&workspace_id)?;
        let repository_id = required_string(&args, "repository_id")?;
        let mut config = self.inner.config.lock().unwrap();
        let workspace = active_workspace_mut(&mut config, &workspace_id)?;
        let before = workspace.repositories.len();
        workspace
            .repositories
            .retain(|repository| repository.id != repository_id);
        if before == workspace.repositories.len() {
            return Err(format!("unknown repository {repository_id:?}"));
        }
        for store in &mut workspace.task_stores {
            if store.repository_id.as_deref() == Some(&repository_id) {
                store.repository_id = None;
                store.source = Some("external".to_owned());
            }
        }
        config::save(&self.inner.data_root, &config).map_err(|error| error.to_string())?;
        Ok(json!({"repository_id": repository_id, "detached": true}))
    }

    fn task_store_attach(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        self.active_runtime(&workspace_id)?;
        let requested = PathBuf::from(required_string(&args, "path")?);
        let mut store = BeadsAdapter::inspect_store(&requested)?;
        store.source = Some("external".to_owned());
        let mut config = self.inner.config.lock().unwrap();
        if let Some(attached) = config
            .workspaces
            .iter()
            .flat_map(|workspace| {
                workspace
                    .task_stores
                    .iter()
                    .map(move |item| (&workspace.id, item))
            })
            .find(|(_, item)| item.db_path == store.db_path)
        {
            if attached.0 == &workspace_id {
                return Ok(json!({"task_store": task_store_view(attached.1), "attached": false}));
            }
            return Err(format!(
                "task store {} is already attached to workspace {}; a canonical database has one Orchard lock owner",
                store.db_path.display(), attached.0
            ));
        }
        let workspace = active_workspace_mut(&mut config, &workspace_id)?;
        store.id = Uuid::new_v4().simple().to_string();
        workspace.task_stores.push(store.clone());
        config::save(&self.inner.data_root, &config).map_err(|error| error.to_string())?;
        self.inner
            .store_locks
            .write()
            .unwrap()
            .entry(store.db_path.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())));
        Ok(json!({"task_store": task_store_view(&store), "attached": true}))
    }

    fn task_store_detach(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        self.active_runtime(&workspace_id)?;
        let store_id = required_string(&args, "store_id")?;
        let mut config = self.inner.config.lock().unwrap();
        let workspace = active_workspace_mut(&mut config, &workspace_id)?;
        let store = workspace
            .task_stores
            .iter()
            .find(|store| store.id == store_id)
            .ok_or_else(|| format!("unknown task store {store_id:?}"))?;
        if task_store_source(store) == "owned" {
            return Err("the workspace-owned task store cannot be detached".to_owned());
        }
        let before = workspace.task_stores.len();
        workspace.task_stores.retain(|store| store.id != store_id);
        if before == workspace.task_stores.len() {
            return Err(format!("unknown task store {store_id:?}"));
        }
        for repository in &mut workspace.repositories {
            if repository.task_store_id.as_deref() == Some(&store_id) {
                repository.task_store_id = None;
                repository.task_status = "none".to_owned();
                repository.task_error = None;
            }
        }
        config::save(&self.inner.data_root, &config).map_err(|error| error.to_string())?;
        Ok(json!({"store_id": store_id, "detached": true}))
    }

    fn tasks_list(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let store_id = required_string(&args, "store_id")?;
        let status = optional_string(&args, "status")?;
        self.with_store(&workspace_id, &store_id, |store| {
            self.inner
                .beads
                .list(store, status.as_deref())
                .map_err(command_error)
        })
    }

    fn task_show(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let store_id = required_string(&args, "store_id")?;
        let task_id = required_string(&args, "task_id")?;
        self.with_store(&workspace_id, &store_id, |store| {
            self.inner
                .beads
                .show(store, &task_id)
                .map_err(command_error)
        })
    }

    fn task_dependencies(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let store_id = required_string(&args, "store_id")?;
        let task_id = required_string(&args, "task_id")?;
        self.with_store(&workspace_id, &store_id, |store| {
            self.inner
                .beads
                .dependencies(store, &task_id)
                .map_err(command_error)
        })
    }

    fn task_create(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let store_id = required_string(&args, "store_id")?;
        let request_id = required_string(&args, "request_id")?;
        let title = required_string(&args, "title")?;
        let description = optional_string(&args, "description")?;
        let priority = optional_u8(&args, "priority")?;
        let labels = optional_strings(&args, "labels")?;
        let semantic = json!({
            "operation":"create","store_id":store_id,"title":title,
            "description":description,"priority":priority,"labels":labels
        });
        self.mutate_task(
            &workspace_id,
            &store_id,
            &request_id,
            "create",
            None,
            semantic,
            |store| {
                self.inner.beads.create(
                    store,
                    CreateTask {
                        workspace_id: &workspace_id,
                        request_id: &request_id,
                        title: &title,
                        description: description.as_deref(),
                        priority,
                        labels: &labels,
                    },
                )
            },
            |store| {
                self.inner
                    .beads
                    .reconcile_create(store, &workspace_id, &request_id)
            },
        )
    }

    fn task_update(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let store_id = required_string(&args, "store_id")?;
        let task_id = required_string(&args, "task_id")?;
        let request_id = required_string(&args, "request_id")?;
        let title = optional_string(&args, "title")?;
        let description = optional_string(&args, "description")?;
        let status = optional_string(&args, "status")?;
        let priority = optional_u8(&args, "priority")?;
        let add_labels = optional_strings(&args, "add_labels")?;
        let remove_labels = optional_strings(&args, "remove_labels")?;
        let semantic = json!({
            "operation":"update","store_id":store_id,"task_id":task_id,"title":title,
            "description":description,"status":status,"priority":priority,
            "add_labels":add_labels,"remove_labels":remove_labels
        });
        self.mutate_task(
            &workspace_id,
            &store_id,
            &request_id,
            "update",
            Some(&task_id),
            semantic,
            |store| {
                self.inner.beads.update(
                    store,
                    &task_id,
                    &request_id,
                    title.as_deref(),
                    description.as_deref(),
                    status.as_deref(),
                    priority,
                    &add_labels,
                    &remove_labels,
                )
            },
            |store| {
                self.inner.beads.reconcile_update(
                    store,
                    &task_id,
                    &request_id,
                    title.as_deref(),
                    description.as_deref(),
                    status.as_deref(),
                    priority,
                    &add_labels,
                    &remove_labels,
                )
            },
        )
    }

    fn task_close(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let store_id = required_string(&args, "store_id")?;
        let task_id = required_string(&args, "task_id")?;
        let request_id = required_string(&args, "request_id")?;
        let reason = optional_string(&args, "reason")?;
        let semantic = json!({
            "operation":"close","store_id":store_id,"task_id":task_id,"reason":reason
        });
        self.mutate_task(
            &workspace_id,
            &store_id,
            &request_id,
            "close",
            Some(&task_id),
            semantic,
            |store| {
                self.inner
                    .beads
                    .close(store, &task_id, &request_id, reason.as_deref())
            },
            |store| {
                self.inner
                    .beads
                    .reconcile_close(store, &task_id, &request_id, reason.as_deref())
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mutate_task<F, R>(
        &self,
        workspace_id: &str,
        store_id: &str,
        request_id: &str,
        operation: &str,
        task_id: Option<&str>,
        semantic_args: Value,
        mutation: F,
        reconcile: R,
    ) -> Result<Value, String>
    where
        F: FnOnce(&TaskStoreConfig) -> Result<Value, CommandFailure>,
        R: Fn(&TaskStoreConfig) -> Result<Option<Value>, CommandFailure>,
    {
        let runtime = self.active_runtime(workspace_id)?;
        let request_lock = {
            let mut locks = self.inner.request_locks.lock().unwrap();
            locks
                .entry((workspace_id.to_owned(), request_id.to_owned()))
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _request_guard = request_lock.lock().unwrap();
        let fingerprint = semantic_fingerprint(&semantic_args)?;
        let receipts = self.task_receipts(&runtime, request_id)?;
        if receipts
            .iter()
            .any(|receipt| receipt.fingerprint != fingerprint)
        {
            return Err(format!(
                "request_id {request_id:?} was already used with different task arguments"
            ));
        }
        if let Some(result) = receipts
            .iter()
            .rev()
            .find(|receipt| receipt.kind == "task_result")
        {
            if result.outcome == "rejected" {
                return Err(result
                    .error
                    .clone()
                    .unwrap_or_else(|| "the earlier task request was rejected".to_owned()));
            }
            let observed = self.with_store_raw(workspace_id, store_id, |store| {
                if let Some(task_id) = result.task_id.as_deref().or(task_id) {
                    self.inner
                        .beads
                        .show(store, task_id)
                        .map(|task| Some(json!({"task": task})))
                } else {
                    reconcile(store)
                }
            });
            return match observed {
                Ok(Some(mut value)) => {
                    if let Value::Object(object) = &mut value {
                        object.insert("request_id".to_owned(), Value::String(request_id.to_owned()));
                        object.insert("idempotent_replay".to_owned(), Value::Bool(true));
                        object.insert(
                            "application_status".to_owned(),
                            Value::String("previous_result_current_observation".to_owned()),
                        );
                    }
                    Ok(value)
                }
                Ok(None) => Err(format!(
                    "request {request_id:?} has a result receipt but its task is no longer observable"
                )),
                Err(error) => Err(error.message),
            };
        }
        if receipts
            .iter()
            .any(|receipt| receipt.kind == "task_intent" || receipt.kind == "task_unknown")
        {
            let observed = self.with_store_raw(workspace_id, store_id, |store| reconcile(store));
            return match observed {
                Ok(Some(value)) => {
                    let observed_task_id = value
                        .pointer("/task/id")
                        .and_then(Value::as_str)
                        .or(task_id);
                    self.record_task_receipt(
                        &runtime,
                        "task_result",
                        request_id,
                        operation,
                        store_id,
                        observed_task_id,
                        &fingerprint,
                        "observed_after_pending",
                        None,
                    )?;
                    Ok(value)
                }
                Ok(None) => Err(format!(
                    "request {request_id:?} has a pending intent; current store state does not prove it applied, so Orchard will not rerun it automatically"
                )),
                Err(error) => Err(error.message),
            };
        }
        self.record_task_receipt(
            &runtime,
            "task_intent",
            request_id,
            operation,
            store_id,
            task_id,
            &fingerprint,
            "pending",
            None,
        )?;
        let outcome = self.with_store_raw(workspace_id, store_id, mutation);
        match outcome {
            Ok(mut value) => {
                let application_status = value
                    .get("application_status")
                    .and_then(Value::as_str)
                    .unwrap_or("observed_without_application_status")
                    .to_owned();
                let receipt_warning = ["command_warning", "persistence_warning"]
                    .into_iter()
                    .filter_map(|key| value.get(key).and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("; ");
                let receipt_error = self
                    .record_task_receipt(
                        &runtime,
                        "task_result",
                        request_id,
                        operation,
                        store_id,
                        value
                            .pointer("/task/id")
                            .and_then(Value::as_str)
                            .or(task_id),
                        &fingerprint,
                        &application_status,
                        (!receipt_warning.is_empty()).then_some(receipt_warning.as_str()),
                    )
                    .err();
                if let (Some(error), Value::Object(object)) = (receipt_error, &mut value) {
                    object.insert("receipt_error".to_owned(), Value::String(error));
                }
                Ok(value)
            }
            Err(error) => {
                let kind = if error.unknown_outcome {
                    "task_unknown"
                } else {
                    "task_result"
                };
                let _ = self.record_task_receipt(
                    &runtime,
                    kind,
                    request_id,
                    operation,
                    store_id,
                    task_id,
                    &fingerprint,
                    if error.unknown_outcome {
                        "unknown"
                    } else {
                        "rejected"
                    },
                    Some(&error.message),
                );
                Err(error.message)
            }
        }
    }

    fn mail_call(&self, operation: &str, args: Value) -> Result<Value, String> {
        let mut args = object(args)?;
        let workspace_id = args
            .remove("workspace_id")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| "mail operation requires string workspace_id".to_owned())?;
        let runtime = self.active_runtime(&workspace_id)?;
        let mail = runtime.mail.as_ref().ok_or_else(|| {
            runtime
                .mail_error
                .clone()
                .unwrap_or_else(|| "mail unavailable".to_owned())
        })?;
        let result = mail
            .lock()
            .unwrap()
            .call(operation, Value::Object(args))
            .map_err(|error| error.to_string());
        result
    }

    fn workspace_snapshot(&self, args: Value) -> Result<Value, String> {
        let args = object(args)?;
        let workspace_id = required_string(&args, "workspace_id")?;
        let history_limit = optional_u64(&args, "history_limit")?.unwrap_or(50);
        let runtime = self.active_runtime(&workspace_id)?;
        let workspace = self.workspace_config(&workspace_id)?;
        let mut errors = Vec::new();

        let mail_snapshot = if let Some(mail) = &runtime.mail {
            let mut mail = mail.lock().unwrap();
            let participants = mail.call("mail_participants", json!({}));
            let channels = mail.call("mail_channels", json!({}));
            let history = mail.call("mail_history", json!({"latest":true,"limit":history_limit}));
            match (participants, channels, history) {
                (Ok(participants), Ok(channels), Ok(history)) => json!({
                    "participants": participants.get("participants").cloned().unwrap_or_else(|| json!([])),
                    "channels": channels.get("channels").cloned().unwrap_or_else(|| json!([])),
                    "history": history.get("messages").cloned().unwrap_or_else(|| json!([]))
                }),
                (participants, channels, history) => {
                    if let Err(error) = participants {
                        errors
                            .push(json!({"source":"mail_participants","error":error.to_string()}));
                    }
                    if let Err(error) = channels {
                        errors.push(json!({"source":"mail_channels","error":error.to_string()}));
                    }
                    if let Err(error) = history {
                        errors.push(json!({"source":"mail_history","error":error.to_string()}));
                    }
                    json!({"participants":[],"channels":[],"history":[]})
                }
            }
        } else {
            errors.push(json!({
                "source": "mail",
                "error": runtime.mail_error.clone().unwrap_or_else(|| "mail unavailable".to_owned())
            }));
            json!({"participants":[],"channels":[],"history":[]})
        };

        let repositories = workspace
            .repositories
            .iter()
            .map(|repository| {
                if !repository.path.exists() {
                    errors.push(json!({
                        "source": "repository",
                        "repository_id": repository.id,
                        "error": format!("{} is missing", repository.path.display())
                    }));
                }
                let store = repository.task_store_id.as_ref().and_then(|store_id| {
                    workspace
                        .task_stores
                        .iter()
                        .find(|store| &store.id == store_id)
                });
                repository_view(repository, store)
            })
            .collect::<Vec<_>>();

        let task_stores = workspace
            .task_stores
            .iter()
            .map(|store| {
                match self.with_store(&workspace_id, &store.id, |store| {
                    self.inner.beads.list(store, None).map_err(command_error)
                }) {
                    Ok(tasks) => json!({
                        "store":task_store_view(store),
                        "tasks":tasks.get("tasks").cloned().unwrap_or_else(|| json!([]))
                    }),
                    Err(error) => {
                        errors
                            .push(json!({"source":"task_store","store_id":store.id,"error":error}));
                        json!({"store":task_store_view(store),"tasks":Value::Null})
                    }
                }
            })
            .collect::<Vec<_>>();

        Ok(json!({
            "workspace": workspace_view(&workspace),
            "mail": mail_snapshot,
            "repositories": repositories,
            "task_stores": task_stores,
            "errors": errors
        }))
    }

    fn settings_get(&self) -> Result<Value, String> {
        let config = self.inner.config.lock().unwrap();
        let visible = json!({
            "version": config.version,
            "port": config.port,
            "data_root": self.inner.data_root,
            "br_version": beads::SUPPORTED_BR_VERSION,
            "beads_schema_version": beads::SUPPORTED_SCHEMA_VERSION,
            "task_backend": task_backend_view(&self.inner.beads),
            "workspaces": config.workspaces.iter().map(workspace_view).collect::<Vec<_>>()
        });
        Ok(json!({
            "format": "json",
            "config": visible,
            "copyable": serde_json::to_string_pretty(&visible).map_err(|error| error.to_string())?
        }))
    }

    fn workspace_info(&self, args: Value) -> Result<Value, String> {
        let workspace_id = workspace_id(&args)?;
        self.active_runtime(&workspace_id)?;
        let workspace = self.workspace_config(&workspace_id)?;
        Ok(json!({
            "workspace": workspace_view(&workspace),
            "owner_participant_id": "owner",
            "system_participant_id": "orchard",
            "capabilities": {
                "mail": MAIL_OPERATIONS,
                "tasks": if self.inner.beads.availability().is_ok() {
                    json!(["tasks_list","task_show","task_create","task_update","task_close","task_dependencies"])
                } else { json!([]) },
                "task_dependencies_mutable": false
            },
            "task_backend": task_backend_view(&self.inner.beads)
        }))
    }

    fn with_store<T, F>(
        &self,
        workspace_id: &str,
        store_id: &str,
        operation: F,
    ) -> Result<T, String>
    where
        F: FnOnce(&TaskStoreConfig) -> Result<T, String>,
    {
        self.with_store_raw(workspace_id, store_id, |store| {
            operation(store).map_err(|message| CommandFailure {
                message,
                unknown_outcome: false,
            })
        })
        .map_err(|error| error.message)
    }

    fn with_store_raw<T, F>(
        &self,
        workspace_id: &str,
        store_id: &str,
        operation: F,
    ) -> Result<T, CommandFailure>
    where
        F: FnOnce(&TaskStoreConfig) -> Result<T, CommandFailure>,
    {
        self.active_runtime(workspace_id)
            .map_err(normal_command_failure)?;
        let store = self
            .workspace_config(workspace_id)
            .map_err(normal_command_failure)?
            .task_stores
            .into_iter()
            .find(|store| store.id == store_id)
            .ok_or_else(|| normal_command_failure(format!("unknown task store {store_id:?}")))?;
        let lock = self
            .inner
            .store_locks
            .read()
            .unwrap()
            .get(&store.db_path)
            .cloned()
            .ok_or_else(|| {
                normal_command_failure(format!("task store {store_id:?} is not active"))
            })?;
        let _guard = lock.lock().unwrap();
        operation(&store)
    }

    fn active_runtime(&self, workspace_id: &str) -> Result<Arc<WorkspaceRuntime>, String> {
        self.inner
            .runtimes
            .read()
            .unwrap()
            .get(workspace_id)
            .cloned()
            .ok_or_else(|| format!("workspace {workspace_id:?} is unknown or archived"))
    }

    fn workspace_config(&self, workspace_id: &str) -> Result<WorkspaceConfig, String> {
        self.inner
            .config
            .lock()
            .unwrap()
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .cloned()
            .ok_or_else(|| format!("unknown workspace {workspace_id:?}"))
    }

    fn rebuild_all_mcp_routers(&self) {
        for runtime in self.inner.runtimes.read().unwrap().values() {
            self.rebuild_mcp_router(runtime);
        }
    }

    fn rebuild_mcp_router(&self, runtime: &Arc<WorkspaceRuntime>) {
        let Some(mail) = runtime.mail.clone() else {
            *runtime.mcp_router.write().unwrap() = None;
            return;
        };
        runtime.mcp_cancellation.lock().unwrap().cancel();
        let cancellation = CancellationToken::new();
        *runtime.mcp_cancellation.lock().unwrap() = cancellation.clone();
        let backend = Arc::new(mcp::CombinedBackend::new(
            Arc::downgrade(&self.inner),
            runtime.id.clone(),
            CoreBackend::new(mail),
        ));
        let token = runtime.token.read().unwrap().clone();
        *runtime.mcp_router.write().unwrap() =
            Some(build_router_with_cancellation(backend, token, cancellation));
    }

    fn initialize_workspace_actors(
        &self,
        runtime: &WorkspaceRuntime,
        owner_name: &str,
    ) -> Result<(), String> {
        let Some(mail) = &runtime.mail else {
            return Err(runtime
                .mail_error
                .clone()
                .unwrap_or_else(|| "mail unavailable".to_owned()));
        };
        let mut mail = mail.lock().unwrap();
        mail.call(
            "mail_register",
            json!({
                "request_id": "orchard-owner-participant-v1",
                "participant_id": "owner",
                "name": owner_name
            }),
        )
        .map_err(|error| error.to_string())?;
        mail.call(
            "mail_register",
            json!({
                "request_id": "orchard-system-participant-v1",
                "participant_id": "orchard",
                "name": "Orchard"
            }),
        )
        .map_err(|error| error.to_string())?;
        mail.call(
            "mail_channel_create",
            json!({
                "request_id": "orchard-task-receipts-channel-v1",
                "channel_id": "task-receipts",
                "name": "Task receipts",
                "description": "Immutable intents and outcomes for Orchard task mutations"
            }),
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_task_receipt(
        &self,
        runtime: &WorkspaceRuntime,
        kind: &str,
        request_id: &str,
        operation: &str,
        store_id: &str,
        task_id: Option<&str>,
        fingerprint: &str,
        outcome: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        let mail = runtime.mail.as_ref().ok_or_else(|| {
            runtime
                .mail_error
                .clone()
                .unwrap_or_else(|| "mail unavailable".to_owned())
        })?;
        let phase = match kind {
            "task_intent" => "intent",
            "task_result" => "result",
            _ => "unknown",
        };
        let receipt_request_id = valid_receipt_request_id(&runtime.id, request_id, phase);
        let reference = json!({
            "type": "orchard_task_receipt",
            "request_id": request_id,
            "operation": operation,
            "fingerprint": fingerprint,
            "outcome": outcome,
            "task_ref": {"store_id": store_id, "task_id": task_id},
            "error": error
        });
        let search_key = receipt_search_key(&runtime.id, request_id);
        let body = format!(
            "Task receipt {search_key} request {request_id} {operation} {phase}: store {store_id}{}",
            task_id.map(|id| format!(", task {id}")).unwrap_or_default()
        );
        mail.lock()
            .unwrap()
            .call(
                "mail_send",
                json!({
                    "request_id": receipt_request_id,
                    "sender_id": "orchard",
                    "destination": {"kind":"channel","id":"task-receipts"},
                    "body": body,
                    "kind": kind,
                    "refs": [reference]
                }),
            )
            .map(|_| ())
            .map_err(|error| format!("could not persist task {phase} receipt: {error}"))
    }

    fn task_receipts(
        &self,
        runtime: &WorkspaceRuntime,
        request_id: &str,
    ) -> Result<Vec<TaskReceipt>, String> {
        let mail = runtime.mail.as_ref().ok_or_else(|| {
            runtime
                .mail_error
                .clone()
                .unwrap_or_else(|| "mail unavailable".to_owned())
        })?;
        let search_key = receipt_search_key(&runtime.id, request_id);
        let result = mail
            .lock()
            .unwrap()
            .call("mail_search", json!({"query":search_key,"limit":200}))
            .map_err(|error| error.to_string())?;
        let mut receipts = Vec::new();
        for message in result
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let kind = message
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(kind, "task_intent" | "task_result" | "task_unknown") {
                continue;
            }
            for reference in message
                .get("refs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if reference.get("type").and_then(Value::as_str) != Some("orchard_task_receipt")
                    || reference.get("request_id").and_then(Value::as_str) != Some(request_id)
                {
                    continue;
                }
                receipts.push(TaskReceipt {
                    kind: kind.to_owned(),
                    fingerprint: reference
                        .get("fingerprint")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    outcome: reference
                        .get("outcome")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    task_id: reference
                        .pointer("/task_ref/task_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    error: reference
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                });
            }
        }
        Ok(receipts)
    }
}

async fn dynamic_mcp(
    State(host): State<Arc<WorkspaceHost>>,
    AxumPath(workspace_id): AxumPath<String>,
    mut request: Request<Body>,
) -> Response {
    let runtime = match host.active_runtime(&workspace_id) {
        Ok(runtime) => runtime,
        Err(message) => return (StatusCode::NOT_FOUND, message).into_response(),
    };
    let router = match runtime.mcp_router.read().unwrap().clone() {
        Some(router) => router,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                runtime
                    .mail_error
                    .clone()
                    .unwrap_or_else(|| "workspace mail is unavailable".to_owned()),
            )
                .into_response()
        }
    };
    *request.uri_mut() = Uri::from_static("/mcp");
    match router.oneshot(request).await {
        Ok(response) => response,
        Err(error) => match error {},
    }
}

#[derive(Deserialize)]
struct SessionLogin {
    token: String,
}

#[derive(Deserialize)]
struct BrowserCall {
    operation: String,
    args: Value,
}

async fn api_session_get(
    State(host): State<Arc<WorkspaceHost>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let authenticated = browser_session_id(&headers).is_some_and(|session| {
        host.inner
            .browser_sessions
            .lock()
            .unwrap()
            .contains_key(&session)
    });
    Json(json!({"authenticated":authenticated}))
}

async fn api_session_post(
    State(host): State<Arc<WorkspaceHost>>,
    headers: HeaderMap,
    Json(login): Json<SessionLogin>,
) -> Response {
    if let Err(error) = validate_browser_mutation(&host, &headers) {
        return browser_validation_response(error);
    }
    if !constant_time_equal(login.token.as_bytes(), host.inner.owner_token.as_bytes()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"invalid owner credential"})),
        )
            .into_response();
    }
    let session = new_token();
    host.inner
        .browser_sessions
        .lock()
        .unwrap()
        .insert(session.clone(), ());
    let cookie = format!("orchard_session={session}; HttpOnly; SameSite=Strict; Path=/api");
    let mut response = Json(json!({"authenticated":true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("generated cookie is valid"),
    );
    response
}

async fn api_session_delete(
    State(host): State<Arc<WorkspaceHost>>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = validate_origin(&host, &headers) {
        return browser_validation_response(error);
    }
    if let Some(session) = browser_session_id(&headers) {
        host.inner.browser_sessions.lock().unwrap().remove(&session);
    }
    let mut response = Json(json!({"authenticated":false})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "orchard_session=; HttpOnly; SameSite=Strict; Path=/api; Max-Age=0",
        ),
    );
    response
}

async fn api_call(
    State(host): State<Arc<WorkspaceHost>>,
    headers: HeaderMap,
    Json(call): Json<BrowserCall>,
) -> Response {
    if let Err(error) = validate_browser_mutation(&host, &headers) {
        return browser_validation_response(error);
    }
    if !browser_authenticated(&host, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"login required"})),
        )
            .into_response();
    }
    let host_for_call = host.clone();
    match tokio::task::spawn_blocking(move || host_for_call.call(&call.operation, call.args)).await
    {
        Ok(Ok(result)) => Json(json!({"result":result})).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":format!("host call stopped unexpectedly: {error}")})),
        )
            .into_response(),
    }
}

type BrowserValidationError = (StatusCode, &'static str);

fn validate_browser_mutation(
    host: &WorkspaceHost,
    headers: &HeaderMap,
) -> Result<(), BrowserValidationError> {
    validate_origin(host, headers)?;
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "application/json is required",
        ));
    }
    Ok(())
}

fn validate_origin(
    host: &WorkspaceHost,
    headers: &HeaderMap,
) -> Result<(), BrowserValidationError> {
    let endpoint = host.inner.endpoint.lock().unwrap();
    let expected = endpoint.map(|endpoint| format!("http://{endpoint}"));
    let observed = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if expected.as_deref() != observed {
        return Err((
            StatusCode::FORBIDDEN,
            "request Origin does not match the Orchard loopback server",
        ));
    }
    Ok(())
}

fn browser_validation_response((status, message): BrowserValidationError) -> Response {
    (status, Json(json!({"error":message}))).into_response()
}

fn browser_authenticated(host: &WorkspaceHost, headers: &HeaderMap) -> bool {
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        return constant_time_equal(token.as_bytes(), host.inner.owner_token.as_bytes());
    }
    browser_session_id(headers).is_some_and(|session| {
        host.inner
            .browser_sessions
            .lock()
            .unwrap()
            .contains_key(&session)
    })
}

fn browser_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == "orchard_session").then(|| value.to_owned())
            })
        })
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && bool::from(left.ct_eq(right))
}

fn load_runtime(
    data_root: &Path,
    workspace: &WorkspaceConfig,
) -> Result<WorkspaceRuntime, HostError> {
    let token = read_token(data_root, &workspace.id)?;
    let (mail, mail_error) = if !workspace.mail_path.exists() {
        (
            None,
            Some(format!(
                "mail repository {} is missing",
                workspace.mail_path.display()
            )),
        )
    } else {
        match MailService::open(&workspace.mail_path) {
            Ok(service) => (Some(Arc::new(Mutex::new(service))), None),
            Err(error) => (None, Some(error.to_string())),
        }
    };
    Ok(WorkspaceRuntime {
        id: workspace.id.clone(),
        token: RwLock::new(token),
        mail,
        mail_error,
        mcp_router: RwLock::new(None),
        mcp_cancellation: Mutex::new(CancellationToken::new()),
    })
}

fn active_workspace_mut<'a>(
    config: &'a mut AppConfig,
    workspace_id: &str,
) -> Result<&'a mut WorkspaceConfig, String> {
    let workspace = config
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == workspace_id)
        .ok_or_else(|| format!("unknown workspace {workspace_id:?}"))?;
    if workspace.archived {
        return Err(format!("workspace {workspace_id:?} is archived"));
    }
    Ok(workspace)
}

fn workspace_view(workspace: &WorkspaceConfig) -> Value {
    json!({
        "id": workspace.id,
        "name": workspace.name,
        "root": workspace.root,
        "archived": workspace.archived,
        "repositories": workspace.repositories.iter().map(|repository| {
            let store = repository.task_store_id.as_ref().and_then(|store_id| {
                workspace.task_stores.iter().find(|store| &store.id == store_id)
            });
            repository_view(repository, store)
        }).collect::<Vec<_>>(),
        "task_stores": workspace.task_stores.iter().map(task_store_view).collect::<Vec<_>>()
    })
}

fn repository_view(repository: &RepositoryConfig, store: Option<&TaskStoreConfig>) -> Value {
    let exists = repository.path.exists();
    let (task_status, task_error) = if !exists {
        (
            "missing",
            Some(format!("{} is missing", repository.path.display())),
        )
    } else if repository.task_store_id.is_some() && store.is_none() {
        (
            "missing",
            Some("the linked task source is no longer attached".to_owned()),
        )
    } else if let Some(store) = store.filter(|store| !store.db_path.exists()) {
        (
            "missing",
            Some(format!("{} is missing", store.db_path.display())),
        )
    } else {
        (
            repository.task_status.as_str(),
            repository.task_error.clone(),
        )
    };
    json!({
        "id":repository.id,
        "path":repository.path,
        "exists":exists,
        "name": if repository.name.is_empty() { repository_name(&repository.path) } else { repository.name.clone() },
        "task_store_id":repository.task_store_id,
        "task_status":task_status,
        "task_error":task_error
    })
}

fn task_store_view(store: &TaskStoreConfig) -> Value {
    json!({
        "id": store.id,
        "name": task_store_name(store),
        "source": task_store_source(store),
        "repository_id": store.repository_id,
        "path": store.path,
        "db_path": store.db_path,
        "schema_version": store.schema_version,
        "exists": store.db_path.exists()
    })
}

struct RepositoryTaskDiscovery {
    status: String,
    error: Option<String>,
    store: Option<TaskStoreConfig>,
}

fn inspect_repository_task_store(repository_root: &Path) -> RepositoryTaskDiscovery {
    let beads_dir = repository_root.join(".beads");
    if !beads_dir.exists() {
        return RepositoryTaskDiscovery {
            status: "none".to_owned(),
            error: None,
            store: None,
        };
    }
    let db_path = beads_dir.join("beads.db");
    if !db_path.is_file() {
        return RepositoryTaskDiscovery {
            status: "missing".to_owned(),
            error: Some(format!(
                "{} exists but has no beads.db",
                beads_dir.display()
            )),
            store: None,
        };
    }
    match BeadsAdapter::inspect_store(&db_path) {
        Ok(store) => RepositoryTaskDiscovery {
            status: "linked".to_owned(),
            error: None,
            store: Some(store),
        },
        Err(error) => RepositoryTaskDiscovery {
            status: "unsupported".to_owned(),
            error: Some(error),
            store: None,
        },
    }
}

fn repository_name(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Repository");
    name.strip_suffix(".git").unwrap_or(name).to_owned()
}

fn task_store_source(store: &TaskStoreConfig) -> &str {
    store.source.as_deref().unwrap_or(if store.id == "default" {
        "owned"
    } else {
        "external"
    })
}

fn task_store_name(store: &TaskStoreConfig) -> String {
    if task_store_source(store) == "owned" {
        "Workspace tasks".to_owned()
    } else {
        repository_name(&store.path)
    }
}

fn task_backend_view(adapter: &BeadsAdapter) -> Value {
    match adapter.availability() {
        Ok(()) => json!({
            "available": true,
            "br_version": beads::SUPPORTED_BR_VERSION,
            "schema_version": beads::SUPPORTED_SCHEMA_VERSION
        }),
        Err(error) => json!({
            "available": false,
            "br_version": beads::SUPPORTED_BR_VERSION,
            "schema_version": beads::SUPPORTED_SCHEMA_VERSION,
            "error": error
        }),
    }
}

fn object(value: Value) -> Result<Map<String, Value>, String> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| "operation arguments must be a JSON object".to_owned())
}

fn workspace_id(value: &Value) -> Result<String, String> {
    let args = object(value.clone())?;
    required_string(&args, "workspace_id")
}

fn required_string(args: &Map<String, Value>, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn optional_string(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(format!("{key} must be a string")),
    }
}

fn optional_strings(args: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("every {key} entry must be a string"))
            })
            .collect(),
        _ => Err(format!("{key} must be an array of strings")),
    }
}

fn optional_u8(args: &Map<String, Value>, key: &str) -> Result<Option<u8>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("{key} must be an unsigned byte")),
    }
}

fn optional_u64(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be an unsigned integer")),
    }
}

fn command_error(error: CommandFailure) -> String {
    error.message
}

fn normal_command_failure(message: impl Into<String>) -> CommandFailure {
    CommandFailure {
        message: message.into(),
        unknown_outcome: false,
    }
}

fn semantic_fingerprint(value: &Value) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
}

fn valid_receipt_request_id(workspace_id: &str, request_id: &str, phase: &str) -> String {
    let digest = Sha256::digest(format!("{workspace_id}\0{request_id}\0{phase}").as_bytes());
    format!("orchard-task-{digest:x}")
}

fn receipt_search_key(workspace_id: &str, request_id: &str) -> String {
    let digest = Sha256::digest(format!("{workspace_id}\0{request_id}").as_bytes());
    format!("receipt-{:x}", digest)
}

fn new_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn create_private_dir(path: &Path) -> Result<(), HostError> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn token_path(data_root: &Path, workspace_id: &str) -> PathBuf {
    data_root
        .join("credentials")
        .join(format!("{workspace_id}.token"))
}

fn write_token(data_root: &Path, workspace_id: &str, token: &str) -> Result<(), HostError> {
    let credentials = data_root.join("credentials");
    create_private_dir(&credentials)?;
    let path = token_path(data_root, workspace_id);
    let temporary = credentials.join(format!("{workspace_id}.token.tmp"));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    use std::io::Write;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_token(data_root: &Path, workspace_id: &str) -> Result<String, HostError> {
    let path = token_path(data_root, workspace_id);
    let token = fs::read_to_string(&path)?.trim().to_owned();
    if token.len() != 64 || !token.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(HostError::Server(format!(
            "credential file {} is invalid",
            path.display()
        )));
    }
    Ok(token)
}

fn read_or_create_owner_token(data_root: &Path) -> Result<(PathBuf, String), HostError> {
    let path = token_path(data_root, "owner");
    if !path.exists() {
        write_token(data_root, "owner", &new_token())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    let token = read_token(data_root, "owner")?;
    Ok((path, token))
}

impl Clone for WorkspaceHost {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

pub(crate) fn call_from_mcp(
    inner: &Weak<HostInner>,
    workspace_id: &str,
    operation: &str,
    mut args: Value,
) -> Result<Value, String> {
    let inner = inner
        .upgrade()
        .ok_or_else(|| "workspace host stopped".to_owned())?;
    let host = WorkspaceHost { inner };
    let object = args
        .as_object_mut()
        .ok_or_else(|| "tool arguments must be a JSON object".to_owned())?;
    object.insert(
        "workspace_id".to_owned(),
        Value::String(workspace_id.to_owned()),
    );
    host.call(operation, args)
}
