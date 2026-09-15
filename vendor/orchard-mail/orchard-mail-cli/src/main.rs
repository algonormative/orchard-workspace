use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use orchard_mail_core::MailService;
use orchard_mail_mcp::{serve_loopback, CoreBackend, ToolBackend};
use rmcp::{
    model::CallToolRequestParams,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Parser)]
#[command(
    name = "orchard-mail",
    version,
    about = "Git-backed mailboxes for cooperating software agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve an authenticated MCP endpoint on 127.0.0.1.
    Serve {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        token_file: PathBuf,
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Call one mail operation through MCP HTTP or in one standalone process.
    Call {
        /// MCP endpoint such as http://127.0.0.1:8000/mcp.
        #[arg(long, conflicts_with = "root", requires = "token_file")]
        url: Option<String>,
        /// Standalone mailbox repository. Opening fails while a server owns it.
        #[arg(long, conflicts_with = "url")]
        root: Option<PathBuf>,
        #[arg(long)]
        token_file: Option<PathBuf>,
        /// One of the canonical mail_* operation names.
        operation: String,
        /// JSON object containing operation arguments.
        args: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse().command {
        Command::Serve {
            root,
            token_file,
            port,
        } => serve(root, token_file, port).await,
        Command::Call {
            url,
            root,
            token_file,
            operation,
            args,
        } => {
            let args: Value = serde_json::from_str(&args).context("args must be a JSON object")?;
            if !args.is_object() {
                bail!("args must be a JSON object");
            }
            let result = match (url, root) {
                (Some(url), None) => {
                    let token =
                        read_token(token_file.as_deref().expect("clap requires token file"))?;
                    call_http(url, token, operation, args).await?
                }
                (None, Some(root)) => MailService::open(root)?.call(&operation, args)?,
                _ => bail!("provide exactly one of --url or --root"),
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
    }
}

async fn serve(root: PathBuf, token_file: PathBuf, port: u16) -> Result<()> {
    let token = read_token(&token_file)?;
    fs::create_dir_all(&root)?;
    reject_token_inside_mailbox(&root, &token_file)?;
    let service = MailService::open(&root)?;
    let backend: Arc<dyn ToolBackend> = Arc::new(CoreBackend::new(Arc::new(Mutex::new(service))));
    let server = serve_loopback(backend, token, port).await?;
    println!(
        "{}",
        serde_json::json!({"url":format!("http://{}/mcp", server.address)})
    );
    tokio::signal::ctrl_c()
        .await
        .context("install Ctrl-C handler")?;
    server.shutdown().await?;
    Ok(())
}

async fn call_http(url: String, token: String, operation: String, args: Value) -> Result<Value> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url).auth_header(token),
    );
    let mut client = ().serve(transport).await.context("connect to MCP server")?;
    let result = client
        .call_tool(
            CallToolRequestParams::new(operation)
                .with_arguments(args.as_object().cloned().unwrap_or_default()),
        )
        .await
        .context("call MCP tool")?;
    let _ = client.close().await;
    if result.is_error == Some(true) {
        let message = result
            .content
            .iter()
            .filter_map(|content| content.as_text())
            .map(|text| text.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        bail!("{message}");
    }
    result
        .structured_content
        .context("MCP response did not contain structured content")
}

fn read_token(path: &Path) -> Result<String> {
    let token =
        fs::read_to_string(path).with_context(|| format!("read token file {}", path.display()))?;
    let token = token.trim().to_owned();
    if token.is_empty() {
        bail!("token file is empty");
    }
    Ok(token)
}

fn reject_token_inside_mailbox(root: &Path, token_file: &Path) -> Result<()> {
    let root = fs::canonicalize(root)?;
    let token = fs::canonicalize(token_file)?;
    if token.starts_with(&root) {
        bail!("token file must live outside the Git-backed mailbox root");
    }
    Ok(())
}
