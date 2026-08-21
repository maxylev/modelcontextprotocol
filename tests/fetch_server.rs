//! End-to-end tests for the fetch server: spawns the real binary over stdio,
//! drives it with the rmcp client, and serves pages from a local HTTP server,
//! mirroring the test coverage of the reference Python server.

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Client, call_tool, text};
use rmcp::{
    model::{ContentBlock, GetPromptRequestParams},
    service::{ClientLifecycleMode, ClientServiceExt},
    transport::TokioChildProcess,
};
use tiny_http::{Header, Response, Server, StatusCode};
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_modelcontextprotocol");
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

const PAGE_HTML: &str = "<!DOCTYPE html><html><head><title>Test Page</title></head>\
<body><article><h1>Hello World</h1><p>This is a test paragraph.</p></article></body></html>";

const BIG_BODY: &str = "The quick brown fox jumps over the lazy dog. ";

/// Mirror of the production raw-response safety limit
/// (`src/fetch/http.rs::MAX_RESPONSE_BODY_BYTES`, 8 MiB).
const RAW_BODY_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
enum RobotsMode {
    /// `User-agent: *\nDisallow: /`
    DisallowAll,
    /// `User-agent: *\nAllow: /`
    AllowAll,
    /// robots.txt returns 404
    Missing,
    /// robots.txt returns 401
    Unauthorized,
    /// robots.txt returns 403
    Forbidden,
    /// robots.txt returns 500
    ServerError,
}

/// A tiny HTTP server that behaves like a static website for the tests.
struct TestServer {
    addr: SocketAddr,
}

impl TestServer {
    fn start(robots: RobotsMode) -> Self {
        let server = Server::http("127.0.0.1:0").expect("bind test server");
        let addr = server.server_addr().to_ip().expect("tcp listener");
        let thread_state = Arc::new(Mutex::new(robots));
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                let response = route(&request, *thread_state.lock().unwrap());
                let _ = request.respond(response);
            }
        });
        Self { addr }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

fn response(status: u16, content_type: &str, body: String) -> Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.into_bytes();
    let headers = vec![
        Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).expect("valid header"),
    ];
    Response::new(
        StatusCode(status),
        headers,
        std::io::Cursor::new(bytes.clone()),
        Some(bytes.len()),
        None,
    )
}

/// Response without a `Content-Length` (chunked transfer), so the client
/// cannot know the size in advance.
fn response_chunked(
    status: u16,
    content_type: &str,
    body: String,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.into_bytes();
    let headers = vec![
        Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).expect("valid header"),
    ];
    Response::new(
        StatusCode(status),
        headers,
        std::io::Cursor::new(bytes),
        None,
        None,
    )
}

/// Response that forces a real `Content-Length` header even for large
/// bodies by raising tiny_http's chunked-encoding threshold well above the
/// body size.
fn response_with_content_length(
    status: u16,
    content_type: &str,
    body: String,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let bytes = body.into_bytes();
    let headers = vec![
        Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).expect("valid header"),
    ];
    Response::new(
        StatusCode(status),
        headers,
        std::io::Cursor::new(bytes.clone()),
        Some(bytes.len()),
        None,
    )
    .with_chunked_threshold(2 * RAW_BODY_LIMIT)
}

fn route(request: &tiny_http::Request, robots: RobotsMode) -> Response<std::io::Cursor<Vec<u8>>> {
    let url = request.url().to_string();
    let user_agent = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("User-Agent"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();

    match url.as_str() {
        "/robots.txt" => match robots {
            RobotsMode::AllowAll => response(200, "text/plain", "User-agent: *\nAllow: /\n".into()),
            RobotsMode::DisallowAll => {
                response(200, "text/plain", "User-agent: *\nDisallow: /\n".into())
            }
            RobotsMode::Missing => response(404, "text/plain", "not found".into()),
            RobotsMode::Unauthorized => response(401, "text/plain", "unauthorized".into()),
            RobotsMode::Forbidden => response(403, "text/plain", "forbidden".into()),
            RobotsMode::ServerError => response(500, "text/plain", "boom".into()),
        },
        "/page" => response(200, "text/html; charset=utf-8", PAGE_HTML.into()),
        "/plain.txt" => response(
            200,
            "text/plain; charset=utf-8",
            "plain text content\n".into(),
        ),
        "/data.json" => response(200, "application/json", "{\"key\": \"value\"}".into()),
        "/big" => {
            let body: String = BIG_BODY.repeat(20);
            response(
                200,
                "text/html; charset=utf-8",
                format!("<html><body><article>{body}</article></body></html>"),
            )
        }
        "/large-allowed" => response(
            200,
            "text/html; charset=utf-8",
            format!(
                "<html><body><article>{}</article></body></html>",
                BIG_BODY.repeat(20_000)
            ),
        ),
        // 8 MiB + 1 with a real Content-Length header.
        "/huge-content-length" => response_with_content_length(
            200,
            "text/html; charset=utf-8",
            format!("OVERSIZED_BODY_START{}", "x".repeat(RAW_BODY_LIMIT + 1)),
        ),
        // 8 MiB + 1 with no Content-Length (chunked/unknown length).
        "/huge-chunked" => response_chunked(
            200,
            "text/html; charset=utf-8",
            format!("OVERSIZED_BODY_START{}", "x".repeat(RAW_BODY_LIMIT + 1)),
        ),
        "/echo-ua" => response(200, "text/plain", user_agent),
        "/missing" => response(404, "text/plain", "not found".into()),
        "/error" => response(500, "text/plain", "server error".into()),
        _ => response(404, "text/plain", "no such route".into()),
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

async fn connect_fetch(args: &[&str]) -> Client {
    let mut cmd = Command::new(BIN);
    cmd.arg("fetch");
    for arg in args {
        cmd.arg(arg);
    }
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn fetch server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("fetch server starts");
    client
}

async fn run_test<F>(future: F) -> F::Output
where
    F: std::future::Future<Output = ()>,
{
    tokio::time::timeout(REQUEST_TIMEOUT, future)
        .await
        .expect("test completed within timeout")
}

fn prompt_text(result: &rmcp::model::GetPromptResult) -> String {
    result
        .messages
        .iter()
        .filter_map(|m| match &m.content {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

// ---------------------------------------------------------------------------
// Server identity / capabilities via discover
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discover_reports_identity_capabilities_and_version() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let info = client.peer_info().expect("discover provides peer info");
        assert_eq!(
            info.protocol_version,
            rmcp::model::ProtocolVersion::V_2026_07_28,
            "negotiates the modern protocol version"
        );
        assert!(
            info.capabilities.tools.is_some(),
            "tools capability advertised"
        );
        assert!(
            info.capabilities.prompts.is_some(),
            "prompts capability advertised"
        );
        assert!(
            info.capabilities.resources.is_none(),
            "no resources capability for the fetch server"
        );

        let implementation = info
            .server_info
            .as_ref()
            .expect("server implementation identity provided");
        assert_eq!(implementation.name, "mcp-fetch");
        assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));

        let instructions = info
            .instructions
            .as_deref()
            .expect("server instructions provided");
        assert!(
            instructions.contains("robots.txt"),
            "instructions explain robots handling: {instructions}"
        );
    })
    .await;
    let _ = &server;
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lists_fetch_tool_with_schema() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let tools = client
            .list_tools(Default::default())
            .await
            .expect("list tools");
        let tool = tools
            .tools
            .iter()
            .find(|t| t.name == "fetch")
            .expect("fetch tool present");
        assert_eq!(tools.tools.len(), 1);
        let description = tool.description.as_deref().expect("fetch description");
        assert!(
            description.contains("https://lite.duckduckgo.com/lite/?q="),
            "got: {description}"
        );
        assert!(description.contains("kp=1"), "got: {description}");
        assert!(description.contains("kp=-1"), "got: {description}");
        assert!(description.contains("kp=-2"), "got: {description}");

        let schema = tool.schema_as_json_value();
        let props = schema["properties"].as_object().expect("properties");
        assert!(props.contains_key("url"), "got: {schema}");
        assert!(props.contains_key("max_length"), "got: {schema}");
        assert!(props.contains_key("start_index"), "got: {schema}");
        assert!(props.contains_key("raw"), "got: {schema}");
        assert_eq!(props["url"]["type"], "string");
        assert_eq!(props["max_length"]["default"], 5000);
        assert_eq!(props["start_index"]["default"], 0);
        assert_eq!(props["raw"]["default"], false);
        // Numeric constraints mirror the runtime validation.
        assert_eq!(props["max_length"]["minimum"], 1, "got: {schema}");
        assert_eq!(props["max_length"]["maximum"], 999999, "got: {schema}");
        assert_eq!(props["start_index"]["minimum"], 0, "got: {schema}");
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&"url".into()));

        // 2026-07-28 cache hints.
        assert_eq!(tools.ttl_ms, Some(0));
        assert_eq!(tools.cache_scope, Some(rmcp::model::CacheScope::Public));
    })
    .await;
    let _ = &server;
}

// ---------------------------------------------------------------------------
// fetch basics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_html_returns_markdown() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let content = text(&result);
        assert!(content.contains("Hello World"), "got: {content}");
        assert!(content.contains("test paragraph"), "got: {content}");
        assert!(content.contains("Contents of"), "got: {content}");
        assert!(
            content.contains(server.base_url().as_str()),
            "got: {content}"
        );
    })
    .await;
}

#[tokio::test]
async fn fetch_raw_returns_original_html() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({
                "url": format!("{}/page", server.base_url()),
                "raw": true
            }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let content = text(&result);
        assert!(content.contains("<html>"), "got: {content}");
        assert!(content.contains("<h1>Hello World</h1>"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn fetch_non_html_returns_raw_with_prefix() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/data.json", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let content = text(&result);
        assert!(
            content.contains("cannot be simplified to markdown"),
            "got: {content}"
        );
        assert!(content.contains("{\"key\": \"value\"}"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn fetch_error_statuses_fail() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/missing", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("status code 404"),
            "got: {}",
            text(&result)
        );

        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/error", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("status code 500"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// robots.txt handling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn robots_txt_disallow_blocks_fetch() {
    let server = TestServer::start(RobotsMode::DisallowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        let content = text(&result);
        assert!(content.contains("robots.txt"), "got: {content}");
        assert!(content.contains("not allowed"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn robots_txt_allow_permits_fetch() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
    })
    .await;
}

#[tokio::test]
async fn robots_txt_missing_permits_fetch() {
    let server = TestServer::start(RobotsMode::Missing);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
    })
    .await;
}

#[tokio::test]
async fn robots_txt_401_and_403_block_fetch() {
    let server = TestServer::start(RobotsMode::Unauthorized);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("assuming that autonomous fetching is not allowed"),
            "got: {}",
            text(&result)
        );
    })
    .await;

    let server2 = TestServer::start(RobotsMode::Forbidden);
    let client2 = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client2,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server2.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("status 403"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

#[tokio::test]
async fn robots_txt_server_error_falls_through_to_parsing() {
    // A 500 from robots.txt is not a 4xx: the reference server parses the
    // body anyway, which contains no rules, so fetching is allowed.
    let server = TestServer::start(RobotsMode::ServerError);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
    })
    .await;
}

#[tokio::test]
async fn ignore_robots_txt_flag_skips_checks() {
    let server = TestServer::start(RobotsMode::DisallowAll);
    let client = connect_fetch(&["--ignore-robots-txt"]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(text(&result).contains("Hello World"));
    })
    .await;
}

// ---------------------------------------------------------------------------
// Truncation / pagination
// ---------------------------------------------------------------------------

#[tokio::test]
async fn max_length_truncates_and_continues() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/big", server.base_url());
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "max_length": 100 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        let first = text(&result);
        assert!(first.contains("Content truncated"), "got: {first}");

        // Follow the continuation hints until the content is fully read.
        let mut start_index: i64 = 0;
        let mut chunks = Vec::new();
        loop {
            let result = call_tool(
                &client,
                "fetch",
                serde_json::json!({ "url": url, "max_length": 100, "start_index": start_index }),
            )
            .await;
            assert_eq!(result.is_error, Some(false));
            let chunk = text(&result);
            assert!(
                !chunk.contains("No more content available."),
                "unexpected end at {start_index}: {chunk}"
            );
            let truncated = chunk.contains("Content truncated");
            chunks.push(chunk.clone());
            if !truncated {
                break;
            }
            let next: i64 = chunk
                .split("start_index of ")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.parse().ok())
                .expect("continuation index in message");
            assert!(next > start_index, "continuation advances: {chunk}");
            start_index = next;
        }

        // The markdown is compact, but a few chunks are expected.
        assert!(chunks.len() >= 2, "got {} chunks: {chunks:?}", chunks.len());
        assert!(chunks.first().unwrap().contains("Content truncated"));
        assert!(!chunks.last().unwrap().contains("Content truncated"));
        assert!(chunks.last().unwrap().contains("The quick brown fox"));
    })
    .await;
}

#[tokio::test]
async fn start_index_past_end_returns_no_more_content() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/page", server.base_url());
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "start_index": 1_000_000 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(
            text(&result).contains("No more content available."),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

// ---------------------------------------------------------------------------
// Argument validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_arguments_are_rejected() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/page", server.base_url());

        // max_length out of range.
        for bad in [0, -5, 1_000_000] {
            let result = call_tool(
                &client,
                "fetch",
                serde_json::json!({ "url": url, "max_length": bad }),
            )
            .await;
            assert_eq!(result.is_error, Some(true), "max_length={bad}");
            assert!(
                text(&result).contains("max_length"),
                "got: {}",
                text(&result)
            );
        }

        // Negative start_index.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "start_index": -1 }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("start_index"));

        // Not a URL.
        let result = call_tool(&client, "fetch", serde_json::json!({ "url": "not a url" })).await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Invalid URL"),
            "got: {}",
            text(&result)
        );

        // Unsupported scheme.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": "ftp://example.com/file" }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("Unsupported URL scheme"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

#[tokio::test]
async fn numeric_boundaries_are_accepted() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/page", server.base_url());

        // Minimum max_length.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "max_length": 1 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        assert!(
            text(&result).contains("Contents of"),
            "got: {}",
            text(&result)
        );

        // Maximum max_length.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "max_length": 999999 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        assert!(
            text(&result).contains("Hello World"),
            "got: {}",
            text(&result)
        );

        // Explicit start_index 0.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "start_index": 0 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
    })
    .await;
}

#[tokio::test]
async fn wrong_or_missing_arguments_are_rejected() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/page", server.base_url());

        // Missing required `url`.
        let result = call_tool(&client, "fetch", serde_json::json!({})).await;
        assert_eq!(result.is_error, Some(true), "got: {result:?}");
        assert!(text(&result).contains("failed to deserialize parameters"));

        // Wrong JSON types.
        for args in [
            serde_json::json!({ "url": 42 }),
            serde_json::json!({ "url": url, "max_length": "many" }),
            serde_json::json!({ "url": url, "start_index": "now" }),
            serde_json::json!({ "url": url, "raw": "yes" }),
        ] {
            let result = call_tool(&client, "fetch", args).await;
            assert_eq!(result.is_error, Some(true), "got: {result:?}");
            assert!(
                text(&result).contains("failed to deserialize parameters"),
                "got: {}",
                text(&result)
            );
        }
    })
    .await;
}

// ---------------------------------------------------------------------------
// Raw response body safety limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oversized_content_length_response_is_rejected() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/huge-content-length", server.base_url());
        // Rejected regardless of the (default) max_length.
        let result = call_tool(&client, "fetch", serde_json::json!({ "url": url })).await;
        assert_eq!(result.is_error, Some(true));
        let content = text(&result);
        assert!(
            content.contains("safety limit"),
            "clear bounded error: {content}"
        );
        assert!(
            !content.contains("OVERSIZED_BODY_START"),
            "remote body leaked into error: {content}"
        );

        // A tiny max_length must NOT make the oversized raw body acceptable:
        // the raw network bound is independent of output truncation.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "max_length": 1 }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(text(&result).contains("safety limit"));
    })
    .await;
}

#[tokio::test]
async fn chunked_unknown_length_response_exceeding_bound_is_rejected() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/huge-chunked", server.base_url());
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "max_length": 10 }),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        let content = text(&result);
        assert!(
            content.contains("safety limit"),
            "chunked oversize rejected while reading: {content}"
        );
        assert!(!content.contains("OVERSIZED_BODY_START"));
    })
    .await;
}

#[tokio::test]
async fn large_body_under_the_raw_bound_succeeds_and_truncation_still_applies() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        // ~460 KB of HTML, far above any max_length but below the 8 MiB raw
        // bound: must succeed and honor character-based max_length.
        let url = format!("{}/large-allowed", server.base_url());
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": url, "max_length": 200 }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        let content = text(&result);
        assert!(content.contains("Content truncated"), "got: {content}");
        assert!(content.contains("The quick brown fox"), "got: {content}");
    })
    .await;
}

// ---------------------------------------------------------------------------
// User agent / prompts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn custom_user_agent_is_used() {
    let server = TestServer::start(RobotsMode::Missing);
    let client = connect_fetch(&["--user-agent", "MyTestAgent/1.0"]).await;
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/echo-ua", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(
            text(&result).contains("MyTestAgent/1.0"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}

#[tokio::test]
async fn default_user_agents_are_used() {
    let server = TestServer::start(RobotsMode::Missing);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        // Tool calls use the autonomous user agent.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/echo-ua", server.base_url()) }),
        )
        .await;
        let content = text(&result);
        assert!(
            content.contains("ModelContextProtocol/1.0 (Autonomous;"),
            "got: {content}"
        );
        // The raw response echoes exactly the UA used for the page request.
        assert!(!content.contains("User-Specified"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn prompts_list_and_get() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let prompts = client
            .list_prompts(Default::default())
            .await
            .expect("list prompts");
        let prompt = prompts
            .prompts
            .iter()
            .find(|p| p.name == "fetch")
            .expect("fetch prompt present");
        assert_eq!(
            prompt.description.as_deref(),
            Some("Fetch a URL and extract its contents as markdown")
        );
        let args = prompt.arguments.as_ref().expect("prompt arguments");
        assert_eq!(args.len(), 1);
        assert_eq!(args[0].name, "url");
        assert_eq!(args[0].required, Some(true));

        let url = format!("{}/page", server.base_url());
        let result = client
            .get_prompt(
                GetPromptRequestParams::new("fetch").with_arguments(
                    serde_json::json!({ "url": url })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("get prompt");
        assert!(result.description.is_some());
        let content = prompt_text(&result);
        assert!(content.contains("Hello World"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn prompts_skip_robots_txt() {
    // Even with a disallowing robots.txt, the user-initiated prompt fetches.
    let server = TestServer::start(RobotsMode::DisallowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/page", server.base_url());
        let result = client
            .get_prompt(
                GetPromptRequestParams::new("fetch").with_arguments(
                    serde_json::json!({ "url": url })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("get prompt");
        let content = prompt_text(&result);
        assert!(content.contains("Hello World"), "got: {content}");
    })
    .await;
}

#[tokio::test]
async fn prompts_use_manual_user_agent() {
    let server = TestServer::start(RobotsMode::Missing);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/echo-ua", server.base_url());
        let result = client
            .get_prompt(
                GetPromptRequestParams::new("fetch").with_arguments(
                    serde_json::json!({ "url": url })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("get prompt");
        let content = prompt_text(&result);
        assert!(
            content.contains("ModelContextProtocol/1.0 (User-Specified;"),
            "got: {content}"
        );
    })
    .await;
}

#[tokio::test]
async fn prompt_missing_url_errors() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let result = client
            .get_prompt(GetPromptRequestParams::new("fetch"))
            .await;
        assert!(result.is_err(), "missing url must error");
    })
    .await;
    let _ = &server;
}

#[tokio::test]
async fn prompt_failed_fetch_reports_error_message() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let client = connect_fetch(&[]).await;
    run_test(async move {
        let url = format!("{}/missing", server.base_url());
        let result = client
            .get_prompt(
                GetPromptRequestParams::new("fetch").with_arguments(
                    serde_json::json!({ "url": url })
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .expect("get prompt");
        let content = prompt_text(&result);
        assert!(content.contains("Failed to fetch"), "got: {content}");
    })
    .await;
}

// ---------------------------------------------------------------------------
// Startup validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invalid_proxy_url_fails_startup() {
    let output = Command::new(BIN)
        .args(["fetch", "--proxy-url", "not a valid proxy url"])
        .output()
        .await
        .expect("binary runs");
    assert!(!output.status.success(), "exit code is non-zero");
}

#[tokio::test]
async fn fetch_flag_form_starts() {
    let server = TestServer::start(RobotsMode::AllowAll);
    let mut cmd = Command::new(BIN);
    cmd.arg("--fetch");
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn fetch server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("flag form starts");
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(text(&result).contains("Hello World"));
    })
    .await;
}

#[tokio::test]
async fn fetch_flag_form_honors_fetch_options() {
    // `--fetch` combined with fetch-specific options must apply them, not
    // silently drop them: with a disallowing robots.txt the ignore flag must
    // take effect, and the custom user agent must be used for requests.
    let server = TestServer::start(RobotsMode::DisallowAll);
    let mut cmd = Command::new(BIN);
    cmd.arg("--fetch")
        .arg("--ignore-robots-txt")
        .arg("--user-agent")
        .arg("FlagFormAgent/2.0");
    let client: Client = ()
        .serve_with_lifecycle(
            TokioChildProcess::new(cmd).expect("spawn fetch server"),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("flag form with options starts");
    run_test(async move {
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/page", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false), "got: {}", text(&result));
        assert!(text(&result).contains("Hello World"));

        // The custom user agent reaches the server.
        let result = call_tool(
            &client,
            "fetch",
            serde_json::json!({ "url": format!("{}/echo-ua", server.base_url()) }),
        )
        .await;
        assert_eq!(result.is_error, Some(false));
        assert!(
            text(&result).contains("FlagFormAgent/2.0"),
            "got: {}",
            text(&result)
        );
    })
    .await;
}
