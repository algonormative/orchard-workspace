use orchard_mail_core::MailService;
use orchard_mail_mcp::{serve_loopback, CoreBackend, ToolBackend};
use rmcp::{
    model::CallToolRequestParams,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

fn arguments(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[tokio::test]
async fn two_official_sdk_clients_share_tools_and_mutations() {
    let temp = TempDir::new().unwrap();
    let service = Arc::new(Mutex::new(MailService::open(temp.path()).unwrap()));
    let backend: Arc<dyn ToolBackend> = Arc::new(CoreBackend::new(service.clone()));
    let server = serve_loopback(backend, "secret".into(), 0).await.unwrap();
    let uri = format!("http://{}/mcp", server.address);

    let transport_a = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(uri.clone()).auth_header("secret"),
    );
    let transport_b = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(uri).auth_header("secret"),
    );
    let (client_a, client_b) = tokio::join!(().serve(transport_a), ().serve(transport_b));
    let client_a = client_a.unwrap();
    let client_b = client_b.unwrap();
    assert_eq!(client_a.list_all_tools().await.unwrap().len(), 11);
    assert_eq!(client_b.list_all_tools().await.unwrap().len(), 11);

    client_a
        .call_tool(
            CallToolRequestParams::new("mail_register").with_arguments(arguments(json!({
                "request_id":"ra", "name":"Alice", "participant_id":"alice"
            }))),
        )
        .await
        .unwrap();
    client_b
        .call_tool(
            CallToolRequestParams::new("mail_register").with_arguments(arguments(json!({
                "request_id":"rb", "name":"Bob", "participant_id":"bob"
            }))),
        )
        .await
        .unwrap();

    let (sent_a, sent_b) = tokio::join!(
        client_a.call_tool(CallToolRequestParams::new("mail_send").with_arguments(arguments(json!({
            "request_id":"sa", "sender_id":"alice", "destination":{"kind":"direct","id":"bob"}, "body":"from a"
        })))),
        client_b.call_tool(CallToolRequestParams::new("mail_send").with_arguments(arguments(json!({
            "request_id":"sb", "sender_id":"bob", "destination":{"kind":"direct","id":"alice"}, "body":"from b"
        }))))
    );
    assert_eq!(sent_a.unwrap().is_error, Some(false));
    assert_eq!(sent_b.unwrap().is_error, Some(false));
    let history = client_a
        .call_tool(
            CallToolRequestParams::new("mail_history")
                .with_arguments(arguments(json!({"latest":true}))),
        )
        .await
        .unwrap();
    assert_eq!(
        history.structured_content.unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    drop(client_a);
    drop(client_b);
    tokio::time::timeout(std::time::Duration::from_secs(4), server.shutdown())
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn auth_origin_rotation_and_retained_client_shutdown() {
    let temp = TempDir::new().unwrap();
    let service = Arc::new(Mutex::new(MailService::open(temp.path()).unwrap()));
    let backend: Arc<dyn ToolBackend> = Arc::new(CoreBackend::new(service));
    let server = serve_loopback(backend.clone(), "old-token".into(), 0)
        .await
        .unwrap();
    let port = server.address.port();
    let uri = format!("http://{}/mcp", server.address);

    let http = reqwest::Client::new();
    assert_eq!(
        http.post(&uri).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.post(&uri)
            .bearer_auth("old-token")
            .header("Origin", "https://evil.example")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let retained = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(uri.clone()).auth_header("old-token"),
    );
    let retained = ().serve(retained).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(4), server.shutdown())
        .await
        .unwrap()
        .unwrap();
    drop(retained);

    let replacement = serve_loopback(backend, "new-token".into(), port)
        .await
        .unwrap();
    assert_eq!(
        http.post(&uri)
            .bearer_auth("old-token")
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let fresh = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(uri).auth_header("new-token"),
    );
    let fresh = ().serve(fresh).await.unwrap();
    assert_eq!(fresh.list_all_tools().await.unwrap().len(), 11);
    drop(fresh);
    replacement.shutdown().await.unwrap();
}
