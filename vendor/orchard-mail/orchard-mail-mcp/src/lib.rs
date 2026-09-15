//! Official `rmcp` Streamable HTTP adapter for Orchard Mail.
//!
//! [`build_router`] returns a relative router whose MCP endpoint is `/mcp`, so
//! desktop hosts can safely nest it under paths such as `/workspaces/{id}`.

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderValue, StatusCode, Uri},
    middleware::{self, Next},
    response::Response,
    Router,
};
use orchard_mail_core::MailService;
use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, RoleServer},
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ErrorData,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    borrow::Cow,
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;

/// Transport-neutral tool description consumed by both embedded hosts and the
/// official `rmcp` adapter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Synchronous backend boundary. The adapter always invokes `call` on
/// `spawn_blocking`, keeping Git and filesystem work off Tokio's event loop.
pub trait ToolBackend: Send + Sync + 'static {
    fn tools(&self) -> Vec<ToolDefinition>;
    fn call(&self, name: &str, args: Value) -> Result<Value, String>;
}

/// Backend backed by a caller-owned shared `MailService`. Embedders reuse the
/// same Arc rather than opening a second writer and violating the lock.
#[derive(Clone)]
pub struct CoreBackend {
    service: Arc<Mutex<MailService>>,
}

impl CoreBackend {
    pub fn new(service: Arc<Mutex<MailService>>) -> Self {
        Self { service }
    }

    pub fn service(&self) -> &Arc<Mutex<MailService>> {
        &self.service
    }

    pub fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    pub fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        self.service
            .lock()
            .map_err(|_| "mail service lock is poisoned".to_owned())?
            .call(name, args)
            .map_err(|e| e.to_string())
    }
}

impl ToolBackend for CoreBackend {
    fn tools(&self) -> Vec<ToolDefinition> {
        CoreBackend::tools(self)
    }

    fn call(&self, name: &str, args: Value) -> Result<Value, String> {
        CoreBackend::call(self, name, args)
    }
}

/// Stable tool catalog shared by standalone and embedded servers.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    let destination = json!({
        "oneOf": [
            {"type":"object","properties":{"kind":{"const":"channel"},"id":{"type":"string"}},"required":["kind","id"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"direct"},"id":{"type":"string"}},"required":["kind","id"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"broadcast"}},"required":["kind"],"additionalProperties":false}
        ]
    });
    vec![
        definition("mail_register", "Register a stable cooperative participant identity. Names are not verified provider identity.", object(
            json!({"request_id":{"type":"string"},"name":{"type":"string","minLength":1},"participant_id":{"type":"string"}}), &["request_id","name"])),
        definition("mail_resume", "Create a new transient session instance for a registered participant.", object(
            json!({"participant_id":{"type":"string"}}), &["participant_id"])),
        definition("mail_leave", "Deregister a participant from future recipient snapshots without deleting history.", object(
            json!({"request_id":{"type":"string"},"participant_id":{"type":"string"}}), &["request_id","participant_id"])),
        definition("mail_participants", "List cooperative identities and process-local observed session presence.", object(json!({}), &[])),
        definition("mail_channel_create", "Create a public delivery channel.", object(
            json!({"request_id":{"type":"string"},"name":{"type":"string","minLength":1},"description":{"type":"string"},"channel_id":{"type":"string"}}), &["request_id","name"])),
        definition("mail_channels", "List public delivery channels.", object(json!({}), &[])),
        definition("mail_send", "Commit an immutable message and snapshot its recipients. Direct messages are cooperative routing, not private encryption.", object(
            json!({"request_id":{"type":"string"},"sender_id":{"type":"string"},"destination":destination,"body":{"type":"string","minLength":1},"thread_id":{"type":"string"},"kind":{"type":"string","default":"message"},"refs":{"type":"array","items":{}}}),
            &["request_id","sender_id","destination","body"])),
        definition("mail_inbox", "Retrieve messages delivered to one participant after an optional sequence cursor.", object(
            json!({"participant_id":{"type":"string"},"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}}), &["participant_id"])),
        definition("mail_history", "Read shared history. Set latest=true to return the newest bounded window in ascending sequence.", object(
            json!({"channel_id":{"type":"string"},"sender_id":{"type":"string"},"thread_id":{"type":"string"},"destination_kind":{"type":"string","enum":["channel","direct","broadcast"]},"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200},"latest":{"type":"boolean","default":false}}), &[])),
        definition("mail_acknowledge", "Persist explicit delivery acknowledgements separately from retrieval.", object(
            json!({"request_id":{"type":"string"},"participant_id":{"type":"string"},"message_ids":{"type":"array","minItems":1,"items":{"type":"string"}}}), &["request_id","participant_id","message_ids"])),
        definition("mail_search", "Case-insensitive search across durable shared message history.", object(
            json!({"query":{"type":"string","minLength":1},"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":200}}), &["query"])),
    ]
}

fn definition(name: &str, description: &str, input_schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema,
    }
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

#[derive(Clone)]
struct DynamicHandler {
    backend: Arc<dyn ToolBackend>,
}

impl DynamicHandler {
    fn rmcp_tools(&self) -> Vec<Tool> {
        self.backend
            .tools()
            .into_iter()
            .map(|definition| {
                let schema: Map<String, Value> = definition
                    .input_schema
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                Tool::new(
                    Cow::Owned(definition.name),
                    Cow::Owned(definition.description),
                    Arc::new(schema),
                )
            })
            .collect()
    }
}

impl ServerHandler for DynamicHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Orchard Mail is a cooperative shared mailbox. Names are not verified identities and direct routing is not confidential.")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: self.rmcp_tools(),
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.rmcp_tools().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let backend = self.backend.clone();
        let name = request.name.into_owned();
        let args = Value::Object(request.arguments.unwrap_or_default());
        match tokio::task::spawn_blocking(move || backend.call(&name, args)).await {
            Ok(Ok(value)) => Ok(CallToolResult::structured(value).into()),
            Ok(Err(message)) => {
                Ok(CallToolResult::error(vec![rmcp::model::ContentBlock::text(message)]).into())
            }
            Err(error) => Ok(CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                format!("mail backend task failed: {error}"),
            )])
            .into()),
        }
    }
}

/// Builds an authenticated Streamable HTTP MCP router at relative path `/mcp`.
/// Requests without an Origin are allowed for native clients. Browser Origins
/// must resolve to an explicit loopback host.
pub fn build_router(backend: Arc<dyn ToolBackend>, token: String) -> Router {
    build_router_with_cancellation(backend, token, CancellationToken::new())
}

/// Builds the same relative `/mcp` router with a caller-owned cancellation
/// token. Embedded hosts should cancel it when rotating or removing a mailbox.
pub fn build_router_with_cancellation(
    backend: Arc<dyn ToolBackend>,
    token: String,
    cancellation: CancellationToken,
) -> Router {
    let service = StreamableHttpService::new(
        move || {
            Ok(DynamicHandler {
                backend: backend.clone(),
            })
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_json_response(true)
            .with_cancellation_token(cancellation),
    );
    Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(move |request, next| {
            authenticate(request, next, token.clone())
        }))
}

async fn authenticate(request: Request<Body>, next: Next, token: String) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if !valid_origin(origin) {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("invalid Origin"))
                .unwrap();
        }
    }
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let authorized = supplied.is_some_and(|candidate| {
        candidate.len() == token.len() && candidate.as_bytes().ct_eq(token.as_bytes()).into()
    });
    if !authorized {
        let mut response = Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(Body::from("missing or invalid bearer token"))
            .unwrap();
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return response;
    }
    next.run(request).await
}

fn valid_origin(value: &HeaderValue) -> bool {
    let Ok(text) = value.to_str() else {
        return false;
    };
    let Ok(uri) = text.parse::<Uri>() else {
        return false;
    };
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return false;
    }
    matches!(uri.host(), Some("localhost" | "127.0.0.1" | "::1"))
}

/// Running loopback-only server. Shutdown allows a three-second graceful drain,
/// then aborts the owned server task so an idle client stream cannot hang exit.
pub struct RunningServer {
    pub address: SocketAddr,
    cancellation: CancellationToken,
    task: Option<JoinHandle<std::io::Result<()>>>,
}

impl RunningServer {
    pub async fn shutdown(mut self) -> std::io::Result<()> {
        self.cancellation.cancel();
        let mut task = self.task.take().expect("server task is present");
        match tokio::time::timeout(std::time::Duration::from_secs(3), &mut task).await {
            Ok(joined) => joined.map_err(std::io::Error::other)?,
            Err(_) => {
                task.abort();
                let _ = task.await;
                Ok(())
            }
        }
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// Starts a loopback server. Port zero requests an ephemeral OS-assigned port.
pub async fn serve_loopback(
    backend: Arc<dyn ToolBackend>,
    token: String,
    port: u16,
) -> std::io::Result<RunningServer> {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let address = listener.local_addr()?;
    let cancellation = CancellationToken::new();
    let shutdown = cancellation.clone();
    let router = build_router_with_cancellation(backend, token, cancellation.child_token());
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
    });
    Ok(RunningServer {
        address,
        cancellation,
        task: Some(task),
    })
}
