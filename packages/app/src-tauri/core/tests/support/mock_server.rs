//! Minimal programmable mock of a Companion-enabled Mix Space Core, ported
//! from `packages/core/test/helpers/mockServer.ts` (spec
//! `protocol-transport.md` section 10). Hand-rolled HTTP/1.1 over a tokio
//! `TcpListener` — no axum/hyper — because the contract needs per-request
//! socket destruction (drop the TCP connection without responding) and raw
//! body byte capture.
//!
//! # API summary (for test authors, incl. agent K)
//!
//! ```ignore
//! let server = MockCompanionServer::start().await;         // 127.0.0.1, ephemeral port
//! let base = server.base_url();                            // "http://127.0.0.1:<port>"
//! server.enqueue(|req| mutation_success(req, None));       // one-shot handler (FIFO)
//! server.enqueue(|_| MockOutcome::SocketDestroy);          // drop the connection -> client network error
//! server.set_fallback(|req| match req.path.as_str() {     // used when the queue is empty
//!     "/companion/capabilities" => capabilities_response(CapabilitiesPatch::default()),
//!     _ => mutation_success(req, None),
//! });
//! let seen = server.requests();                            // snapshot, arrival order
//! server.stop().await;                                     // optional; Drop also shuts down
//! ```
//!
//! Handlers return anything `Into<MockOutcome>`: a [`MockResponse`]
//! `{ status, body }` (sent as `Content-Type: application/json` +
//! compact-serialized body) or [`MockOutcome::SocketDestroy`]. With no
//! queued handler and no fallback, the server answers
//! `500 {"unexpected":true}`. Every request is recorded (in arrival order)
//! BEFORE the handler runs, as [`RecordedRequest`] `{ method, path,
//! headers (lowercased names), raw_body (utf8), json (parsed or None) }`.
//!
//! The response builders ([`response_meta`], [`mutation_success`],
//! [`error_envelope`], [`capabilities_response`]) mirror the mockServer.ts
//! literals byte-for-byte, including `NOW`.
//!
//! Deviation from the Node mock: every response carries `Connection: close`
//! (one connection per request). Node kept connections alive; the difference
//! is invisible to the client contract and keeps hyper's pooled-connection
//! retry out of the picture, so "server saw exactly N requests" assertions
//! stay exact.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// One captured HTTP request, mirroring the TS `RecordedRequest`.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    /// Raw request-target (path + query), like Node's `req.url`.
    pub path: String,
    /// Header names lowercased; a duplicate name keeps the LAST value.
    pub headers: HashMap<String, String>,
    /// UTF-8 (lossy) request body text; empty string for bodyless requests.
    pub raw_body: String,
    /// Parsed body; `None` when empty or not JSON (TS used `null`).
    pub json: Option<Value>,
}

/// `{ status, body }` handler outcome (body is serialized compact, exactly
/// like `JSON.stringify`).
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub body: Value,
}

/// What a handler tells the server to do with the connection.
#[derive(Debug, Clone)]
pub enum MockOutcome {
    Respond(MockResponse),
    /// Destroy the TCP socket without responding — the client observes this
    /// as an ambiguous network failure.
    SocketDestroy,
}

impl From<MockResponse> for MockOutcome {
    fn from(response: MockResponse) -> Self {
        MockOutcome::Respond(response)
    }
}

type Handler = Box<dyn Fn(&RecordedRequest) -> MockOutcome + Send>;

#[derive(Default)]
struct ServerState {
    requests: Vec<RecordedRequest>,
    /// FIFO queue of one-shot handlers, consumed in request-arrival order.
    handlers: Vec<Handler>,
    fallback: Option<Handler>,
}

pub struct MockCompanionServer {
    port: u16,
    state: Arc<Mutex<ServerState>>,
    shutdown: Option<oneshot::Sender<()>>,
    accept_task: Option<JoinHandle<()>>,
}

impl MockCompanionServer {
    /// Bind `127.0.0.1` on an ephemeral port and start accepting.
    pub async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("mock server: bind 127.0.0.1 ephemeral port");
        let port = listener
            .local_addr()
            .expect("mock server: read bound address")
            .port();
        let state = Arc::new(Mutex::new(ServerState::default()));
        let (shutdown, mut shutdown_rx) = oneshot::channel::<()>();
        let accept_state = Arc::clone(&state);
        let accept_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        tokio::spawn(handle_connection(stream, Arc::clone(&accept_state)));
                    }
                }
            }
        });
        Self {
            port,
            state,
            shutdown: Some(shutdown),
            accept_task: Some(accept_task),
        }
    }

    /// `http://127.0.0.1:<port>` — loopback HTTP is allowed by the client's
    /// base-URL rules.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Queue a one-shot handler (consumed in order).
    pub fn enqueue<O, F>(&self, handler: F)
    where
        O: Into<MockOutcome>,
        F: Fn(&RecordedRequest) -> O + Send + 'static,
    {
        self.lock()
            .handlers
            .push(Box::new(move |req| handler(req).into()));
    }

    /// Handler used whenever the one-shot queue is empty.
    pub fn set_fallback<O, F>(&self, handler: F)
    where
        O: Into<MockOutcome>,
        F: Fn(&RecordedRequest) -> O + Send + 'static,
    {
        self.lock().fallback = Some(Box::new(move |req| handler(req).into()));
    }

    /// Snapshot of every request seen so far, in arrival order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.lock().requests.clone()
    }

    /// Stop accepting and wait for the accept loop to exit. In-flight
    /// connection tasks finish on their own.
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.accept_task.take() {
            let _ = task.await;
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ServerState> {
        self.state.lock().expect("mock server state poisoned")
    }
}

impl Drop for MockCompanionServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.accept_task.take() {
            task.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Connection handling (one HTTP/1.1 request per connection)
// ---------------------------------------------------------------------------

async fn handle_connection(mut stream: TcpStream, state: Arc<Mutex<ServerState>>) {
    let Ok(recorded) = read_request(&mut stream).await else {
        return;
    };
    // Record + consume the handler under ONE lock so arrival order and
    // handler-queue order stay consistent under concurrent connections.
    let outcome = {
        let mut state = state.lock().expect("mock server state poisoned");
        state.requests.push(recorded.clone());
        if state.handlers.is_empty() {
            match &state.fallback {
                Some(fallback) => fallback(&recorded),
                None => MockOutcome::Respond(MockResponse {
                    status: 500,
                    body: json!({ "unexpected": true }),
                }),
            }
        } else {
            let handler = state.handlers.remove(0);
            handler(&recorded)
        }
    };
    match outcome {
        MockOutcome::SocketDestroy => {
            // Drop the connection without writing a single response byte.
            // Node's `socket.destroy()` after the request was fully read
            // also just closes the socket; the waiting client sees the
            // connection die before a status line and surfaces a network
            // error (never an empty/partial response).
            drop(stream);
        }
        MockOutcome::Respond(response) => {
            let body = response.body.to_string();
            let head = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.status,
                reason_phrase(response.status),
                body.len(),
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
        }
    }
}

/// Parse one HTTP/1.1 request (request line + headers + `Content-Length`
/// body). The client only ever sends length-delimited bodies (reqwest sets
/// `Content-Length` for byte-buffer bodies); chunked encoding is not
/// supported.
async fn read_request(stream: &mut TcpStream) -> std::io::Result<RecordedRequest> {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let header_end = loop {
        if let Some(position) = find_header_end(&buffer) {
            break position;
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before request head",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; content_length - body.len()];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }

    let raw_body = String::from_utf8_lossy(&body).into_owned();
    let json = if raw_body.is_empty() {
        None
    } else {
        serde_json::from_str(&raw_body).ok()
    };
    Ok(RecordedRequest {
        method,
        path,
        headers,
        raw_body,
        json,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        426 => "Upgrade Required",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

// ---------------------------------------------------------------------------
// Response builders (exact literals of mockServer.ts)
// ---------------------------------------------------------------------------

/// `serverTime` / `receivedAt` used by every built response.
pub const NOW: &str = "2026-07-26T04:00:12.345Z";

pub fn response_meta(request_id: &str) -> Value {
    json!({
        "schema": "yohaku.companion.presence",
        "schemaVersion": 2,
        "requestId": request_id,
        "serverTime": NOW,
    })
}

/// Echo of `req.json.meta.requestId`, else a fresh random UUID.
fn echoed_request_id(req: &RecordedRequest) -> String {
    req.json
        .as_ref()
        .and_then(|body| body.get("meta"))
        .and_then(|meta| meta.get("requestId"))
        .and_then(|id| id.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

/// 200 mutation success. `acceptedSequence` defaults to the request's
/// `meta.sequence`, else `0`.
pub fn mutation_success(req: &RecordedRequest, accepted_sequence: Option<u64>) -> MockResponse {
    let request_sequence = req
        .json
        .as_ref()
        .and_then(|body| body.get("meta"))
        .and_then(|meta| meta.get("sequence"))
        .and_then(Value::as_u64);
    MockResponse {
        status: 200,
        body: json!({
            "meta": response_meta(&echoed_request_id(req)),
            "data": {
                "acceptedSequence": accepted_sequence.or(request_sequence).unwrap_or(0),
                "receivedAt": NOW,
                "state": {
                    "schemaVersion": 2,
                    "epoch": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                    "revision": 1,
                    "projection": null,
                },
            },
        }),
    }
}

/// Options for [`error_envelope`] (TS optional fields; `None` -> defaults
/// `retryable: false`, `acceptedSequence: null`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ErrorEnvelopeOptions {
    pub retryable: Option<bool>,
    pub accepted_sequence: Option<u64>,
}

/// Protocol error envelope with the given status and code.
pub fn error_envelope(
    req: &RecordedRequest,
    status: u16,
    code: &str,
    options: ErrorEnvelopeOptions,
) -> MockResponse {
    MockResponse {
        status,
        body: json!({
            "meta": response_meta(&echoed_request_id(req)),
            "error": {
                "code": code,
                "message": format!("{code} for testing"),
                "retryable": options.retryable.unwrap_or(false),
                "retryAfterMs": null,
                "acceptedSequence": options.accepted_sequence,
                "fields": [],
            },
        }),
    }
}

/// Patch for [`capabilities_response`] (TS optional fields; `None` keeps the
/// mockServer.ts defaults).
#[derive(Debug, Clone, Default)]
pub struct CapabilitiesPatch {
    pub minimum_client_version: Option<String>,
    pub live_desk: Option<bool>,
    pub media_timeline: Option<bool>,
    pub presence_schema_versions: Option<Vec<i64>>,
    pub requests_per_minute: Option<u64>,
}

/// 200 capabilities response (fresh random requestId — capabilities carry no
/// echo).
pub fn capabilities_response(patch: CapabilitiesPatch) -> MockResponse {
    MockResponse {
        status: 200,
        body: json!({
            "meta": response_meta(&uuid::Uuid::new_v4().to_string()),
            "data": {
                "minimumClientVersion": patch.minimum_client_version.as_deref().unwrap_or("1.7.0"),
                "presenceSchemaVersions": patch.presence_schema_versions.unwrap_or_else(|| vec![2]),
                "momentSchemaVersions": [1],
                "features": {
                    "liveDesk": patch.live_desk.unwrap_or(true),
                    "mediaTimeline": patch.media_timeline.unwrap_or(true),
                    "moments": true,
                    "readingSessions": false,
                },
                "limits": {
                    "presencePayloadBytes": 32768,
                    "presenceRequestsPerMinute": patch.requests_per_minute.unwrap_or(120),
                    "presenceLeaseMinSeconds": 30,
                    "presenceLeaseMaxSeconds": 120,
                    "recommendedHeartbeatSeconds": 45,
                    "maximumClockSkewSeconds": 60,
                },
            },
        }),
    }
}
