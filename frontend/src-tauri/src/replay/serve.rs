//! `replay serve`: the replay viewer, the local runs and a live Copilot, over
//! HTTP on 127.0.0.1 only. A developer tool: built with the `dev-replay`
//! feature only, so release builds carry no server and no copy of the viewer.
//!
//! - `/replay/…` serves the viewer (embedded from `site/replay` at build time).
//! - `GET /api/runs` lists the runs under the replays folder, newest first.
//! - `GET /api/runs/<run>/events` and `/report` return a run's files;
//!   `/interactive` the questions asked live in the viewer.
//! - `GET /api/runs/<run>/tail?from=<line>` streams a run's events as
//!   server-sent events, following the file while the run is still writing it.
//! - `POST /api/runs/<run>/copilot` asks the real Copilot about the meeting as
//!   it stood at a cursor (see [`super::live_copilot`]) and streams the answer
//!   as server-sent events; `POST /api/runs/<run>/copilot/stop` stops it.
//!
//! Local and as read-only as it can be: it binds to the loopback address,
//! answers only requests addressed to it by that name (so a web page cannot
//! reach it through DNS rebinding), sends no CORS headers, refuses POSTs from
//! other origins or without a JSON body (so a page elsewhere cannot make the
//! browser send one), serves only files it embeds or the files of a run folder
//! directly under the replays root, and writes nothing but the run's
//! `interactive.jsonl`. The only network requests it makes are the Copilot
//! model's, to the provider the run's sandbox settings name (the built-in
//! model by default, which stays on this machine).

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub use super::live_ask::live_config;
use super::live_ask::LiveAsker;
pub use super::live_copilot::Cutoff;
pub use super::runs::{list_runs, parse_ask, run_audio, run_dir, valid_run_name, Ask, AskBody};

pub const DEFAULT_PORT: u16 = 7357;

const ASSETS: &[(&str, &str, &str)] = &[
    (
        "index.html",
        "text/html; charset=utf-8",
        include_str!("../../../../site/replay/index.html"),
    ),
    (
        "boot.js",
        "text/javascript; charset=utf-8",
        include_str!("../../../../site/replay/boot.js"),
    ),
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_str!("../../../../site/replay/app.js"),
    ),
    (
        "parser.js",
        "text/javascript; charset=utf-8",
        include_str!("../../../../site/replay/parser.js"),
    ),
    (
        "replay.css",
        "text/css; charset=utf-8",
        include_str!("../../../../site/replay/replay.css"),
    ),
    (
        "sample-data.js",
        "text/javascript; charset=utf-8",
        include_str!("../../../../site/replay/sample-data.js"),
    ),
];

const FONTS: &[(&str, &[u8])] = &[
    (
        "source-sans-3-latin-variable.woff2",
        include_bytes!("../../../../site/fonts/source-sans-3-latin-variable.woff2"),
    ),
    (
        "source-serif-4-latin-variable.woff2",
        include_bytes!("../../../../site/fonts/source-serif-4-latin-variable.woff2"),
    ),
];

/// The most a POST body may be.
const MAX_BODY: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Paths and hosts
// ---------------------------------------------------------------------------

/// Whether the `Host` header names this server (loopback, this port).
pub fn allowed_host(host: Option<&str>, port: u16) -> bool {
    matches!(host, Some(h) if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

/// Whether a POST may change something: it comes from this server's own
/// page (or from no page at all, like curl) and carries JSON, which a
/// cross-origin page cannot send without a preflight this server never
/// answers.
pub fn allowed_post(origin: Option<&str>, content_type: Option<&str>, port: u16) -> bool {
    let same_origin = match origin {
        None => true,
        Some(o) => {
            o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
        }
    };
    let json = content_type
        .map(|c| {
            c.split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
        .unwrap_or(false);
    same_origin && json
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Request {
    method: String,
    path: String,
    query: String,
    host: Option<String>,
    origin: Option<String>,
    content_type: Option<String>,
    range: Option<String>,
    body: Vec<u8>,
}

/// The longest request or header line read.
const MAX_LINE: usize = 8 * 1024;
/// The most all header lines together may be.
const MAX_HEADERS: usize = 16 * 1024;

/// One request or header line, refused once it passes `MAX_LINE` bytes
/// without a newline (so one endless line cannot fill memory).
fn read_line_capped(reader: &mut impl BufRead, line: &mut String) -> Result<usize> {
    let read = Read::take(&mut *reader, MAX_LINE as u64 + 1).read_line(line)?;
    if read > MAX_LINE {
        return Err(anyhow!("header line too long"));
    }
    Ok(read)
}

fn read_request(stream: &TcpStream) -> Result<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    read_request_from(&mut reader)
}

fn read_request_from(reader: &mut impl BufRead) -> Result<Request> {
    let mut first = String::new();
    read_line_capped(reader, &mut first)?;
    let mut parts = first.split_whitespace();
    let mut request = Request {
        method: parts.next().unwrap_or_default().to_string(),
        ..Default::default()
    };
    let target = parts.next().unwrap_or_default().to_string();
    let mut length = 0usize;
    let mut total = first.len();
    loop {
        let mut line = String::new();
        let read = read_line_capped(reader, &mut line)?;
        total += read;
        if read == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        if total > MAX_HEADERS {
            return Err(anyhow!("headers too large"));
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim().to_string();
            match name.trim().to_ascii_lowercase().as_str() {
                "host" => request.host = Some(value),
                "origin" => request.origin = Some(value),
                "content-type" => request.content_type = Some(value),
                "range" => request.range = Some(value),
                "content-length" => length = value.parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if length > MAX_BODY {
        return Err(anyhow!("body too large"));
    }
    if length > 0 {
        request.body = vec![0; length];
        reader.read_exact(&mut request.body)?;
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    request.path = path.to_string();
    request.query = query.to_string();
    Ok(request)
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}

fn respond_json(stream: &mut TcpStream, status: &str, value: &Value) -> Result<()> {
    respond(
        stream,
        status,
        "application/json",
        &serde_json::to_vec(value)?,
    )
}

fn not_found(stream: &mut TcpStream) -> Result<()> {
    respond(stream, "404 Not Found", "text/plain", b"Not found")
}

fn start_event_stream(stream: &mut TcpStream) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    )
}

/// One server-sent event. Every line of `data` gets its own `data:` field,
/// so the browser joins them back with newlines.
pub fn sse_frame(event: Option<&str>, data: &str) -> String {
    let mut frame = String::new();
    if let Some(event) = event {
        frame.push_str("event: ");
        frame.push_str(event);
        frame.push('\n');
    }
    for line in data.split('\n') {
        frame.push_str("data: ");
        frame.push_str(line.strip_suffix('\r').unwrap_or(line));
        frame.push('\n');
    }
    frame.push('\n');
    frame
}

fn send_event(stream: &mut TcpStream, event: &str, data: &Value) -> std::io::Result<()> {
    stream.write_all(sse_frame(Some(event), &data.to_string()).as_bytes())?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

fn is_end_record(line: &str) -> bool {
    line.contains("\"type\":\"meta\"")
        && (line.contains("\"name\":\"run-end\"") || line.contains("\"name\":\"run-error\""))
}

/// Streams `events.jsonl` from line `from` as server-sent events, following
/// the file until the run ends (or the viewer goes away).
fn tail(stream: &mut TcpStream, dir: &Path, from: usize) -> Result<()> {
    start_event_stream(stream)?;
    let path = dir.join("events.jsonl");
    let mut offset = 0u64;
    let mut pending = String::new();
    let mut line_no = 0usize;
    let mut last_send = Instant::now();
    loop {
        let mut file = std::fs::File::open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = Vec::new();
        let read = file.read_to_end(&mut chunk)?;
        offset += read as u64;
        pending.push_str(&String::from_utf8_lossy(&chunk));
        let mut ended = false;
        let mut out = String::new();
        while let Some(newline) = pending.find('\n') {
            let line: String = pending.drain(..=newline).collect();
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            if line_no >= from {
                out.push_str(&sse_frame(None, line));
            }
            line_no += 1;
            ended |= is_end_record(line);
        }
        if !out.is_empty() {
            stream.write_all(out.as_bytes())?;
            last_send = Instant::now();
        }
        let finished = dir.join("report.json").exists() && read == 0;
        if ended || finished {
            stream.write_all(sse_frame(Some("end"), "{}").as_bytes())?;
            return Ok(());
        }
        if last_send.elapsed() > Duration::from_secs(15) {
            stream.write_all(b": keep-alive\n\n")?;
            last_send = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn query_number(query: &str, key: &str) -> usize {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// The recording
// ---------------------------------------------------------------------------

/// A `Range` header against a body of `len` bytes: `Ok(None)` for the whole
/// body (no header, or one this server does not honour, such as several
/// ranges), `Ok(Some((first, last)))` inclusive, `Err(())` when unsatisfiable
/// (416).
pub fn parse_range(header: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Ok(None);
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        // The last N bytes.
        let Ok(n) = last.parse::<u64>() else {
            return Ok(None);
        };
        if n == 0 || len == 0 {
            return Err(());
        }
        return Ok(Some((len.saturating_sub(n), len - 1)));
    }
    let Ok(first) = first.parse::<u64>() else {
        return Ok(None);
    };
    let last = if last.is_empty() {
        u64::MAX
    } else {
        match last.parse::<u64>() {
            Ok(last) if last >= first => last,
            _ => return Ok(None),
        }
    };
    if first >= len {
        return Err(());
    }
    Ok(Some((first, last.min(len - 1))))
}

/// Streams the run's recording, honouring `Range` so the browser can seek.
fn serve_audio(stream: &mut TcpStream, dir: &Path, range: Option<&str>) -> Result<()> {
    let (path, kind) = match run_audio(dir) {
        Ok(found) => found,
        Err(message) => {
            return respond(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                message.as_bytes(),
            )
        }
    };
    let mut file = std::fs::File::open(&path)?;
    let len = file.metadata()?.len();
    let head = |status: &str, length: u64, extra: String| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {length}\r\n\
             Accept-Ranges: bytes\r\n{extra}Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
             Connection: close\r\n\r\n"
        )
    };
    match parse_range(range, len) {
        Err(()) => {
            write!(
                stream,
                "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{len}\r\n\
                 Accept-Ranges: bytes\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
        }
        Ok(None) => {
            stream.write_all(head("200 OK", len, String::new()).as_bytes())?;
            std::io::copy(&mut file, stream)?;
        }
        Ok(Some((first, last))) => {
            let length = last - first + 1;
            stream.write_all(
                head(
                    "206 Partial Content",
                    length,
                    format!("Content-Range: bytes {first}-{last}/{len}\r\n"),
                )
                .as_bytes(),
            )?;
            file.seek(SeekFrom::Start(first))?;
            std::io::copy(&mut file.take(length), stream)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Live Copilot
// ---------------------------------------------------------------------------

/// State shared by the server's connections for live Copilot asks.
struct Live {
    runtime: tokio::runtime::Runtime,
    asker: LiveAsker,
}

impl Live {
    fn new() -> Result<Self> {
        Ok(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?,
            asker: LiveAsker::new(),
        })
    }

    fn stop(&self, run: &str) -> bool {
        self.asker.stop(run)
    }
}

/// Streams one live Copilot answer as server-sent events (see
/// [`super::live_ask`]): `queued`, `context`, `warming`, `start`, `delta`,
/// `first-token`, `done` (with the saved entry) or `error`.
fn copilot(stream: &mut TcpStream, live: &Live, run: &str, dir: &Path, body: &[u8]) -> Result<()> {
    let ask = match parse_ask(body) {
        Ok(ask) => ask,
        Err(error) => return respond_json(stream, "400 Bad Request", &json!({ "error": error })),
    };
    start_event_stream(stream)?;
    let sink = |event: &str, data: Value| {
        if event == "ping" {
            // Also how a closed tab is noticed: the write fails.
            stream.write_all(b": ping\n\n").is_ok()
        } else {
            send_event(stream, event, &data).is_ok()
        }
    };
    live.runtime.block_on(live.asker.ask(run, dir, ask, sink))
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

#[cfg(test)]
fn handle(stream: TcpStream, root: &Path, port: u16, live: &Live) -> Result<()> {
    handle_with(stream, root, port, live, None)
}

fn handle_with(
    mut stream: TcpStream,
    root: &Path,
    port: u16,
    live: &Live,
    site: Option<&Path>,
) -> Result<()> {
    let request = match read_request(&stream) {
        Ok(request) => request,
        Err(_) => return respond(&mut stream, "400 Bad Request", "text/plain", b"Bad request"),
    };
    if !allowed_host(request.host.as_deref(), port) {
        return respond(&mut stream, "403 Forbidden", "text/plain", b"Forbidden");
    }
    let path = request.path.as_str();
    if request.method == "POST" {
        if !allowed_post(
            request.origin.as_deref(),
            request.content_type.as_deref(),
            port,
        ) {
            return respond(&mut stream, "403 Forbidden", "text/plain", b"Forbidden");
        }
        let Some(rest) = path.strip_prefix("/api/runs/") else {
            return not_found(&mut stream);
        };
        let Some((name, action)) = rest.split_once('/') else {
            return not_found(&mut stream);
        };
        let Some(dir) = run_dir(root, name) else {
            return not_found(&mut stream);
        };
        return match action {
            "copilot" => copilot(&mut stream, live, name, &dir, &request.body),
            "copilot/stop" => {
                let stopped = live.stop(name);
                respond_json(&mut stream, "200 OK", &json!({ "stopped": stopped }))
            }
            _ => not_found(&mut stream),
        };
    }
    if request.method != "GET" {
        return respond(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"Method not allowed",
        );
    }
    if path == "/" || path == "/replay" {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: /replay/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    if let Some(asset) = path.strip_prefix("/replay/") {
        let asset = if asset.is_empty() {
            "index.html"
        } else {
            asset
        };
        return match ASSETS.iter().find(|(name, _, _)| *name == asset) {
            // `--site`: the viewer's files from disk, for working on it.
            Some((name, kind, _)) if site.is_some() => {
                match std::fs::read(site.unwrap_or(root).join(name)) {
                    Ok(body) => respond(&mut stream, "200 OK", kind, &body),
                    Err(_) => not_found(&mut stream),
                }
            }
            Some((_, kind, body)) => respond(&mut stream, "200 OK", kind, body.as_bytes()),
            None => not_found(&mut stream),
        };
    }
    if let Some(font) = path.strip_prefix("/fonts/") {
        return match FONTS.iter().find(|(name, _)| *name == font) {
            Some((_, body)) => respond(&mut stream, "200 OK", "font/woff2", body),
            None => not_found(&mut stream),
        };
    }
    if path == "/api/runs" {
        return respond_json(&mut stream, "200 OK", &list_runs(root));
    }
    if let Some(rest) = path.strip_prefix("/api/runs/") {
        let Some((name, what)) = rest.split_once('/') else {
            return not_found(&mut stream);
        };
        let Some(dir) = run_dir(root, name) else {
            return not_found(&mut stream);
        };
        let file =
            |name: &str, kind: &str, stream: &mut TcpStream| match std::fs::read(dir.join(name)) {
                Ok(body) => respond(stream, "200 OK", kind, &body),
                Err(_) => not_found(stream),
            };
        return match what {
            "events" => file("events.jsonl", "application/x-ndjson", &mut stream),
            "report" => file("report.json", "application/json", &mut stream),
            "audio" => serve_audio(&mut stream, &dir, request.range.as_deref()),
            "interactive" => match std::fs::read(dir.join("interactive.jsonl")) {
                Ok(body) => respond(&mut stream, "200 OK", "application/x-ndjson", &body),
                Err(_) => respond(&mut stream, "200 OK", "application/x-ndjson", b""),
            },
            "tail" => tail(&mut stream, &dir, query_number(&request.query, "from")),
            _ => not_found(&mut stream),
        };
    }
    not_found(&mut stream)
}

/// Binds the server's socket: the loopback address only.
pub fn bind(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| anyhow!("could not listen on 127.0.0.1:{port}: {e}"))
}

/// Serves the viewer and the runs under `root` until killed. `site` serves
/// the viewer from that folder instead of the copy built in.
pub fn serve(root: PathBuf, port: u16, open: bool, site: Option<PathBuf>) -> Result<()> {
    std::fs::create_dir_all(&root)?;
    let listener = bind(port)?;
    let live = Arc::new(Live::new()?);
    let url = format!("http://127.0.0.1:{port}/replay/");
    println!("Replay viewer: {url}");
    println!(
        "Runs: {} (this machine only; live Copilot asks are saved to each run's interactive.jsonl; Ctrl-C to stop)",
        root.display()
    );
    if open {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let root = root.clone();
        let live = live.clone();
        let site = site.clone();
        std::thread::spawn(move || {
            if let Err(error) = handle_with(stream, &root, port, &live, site.as_deref()) {
                log::debug!("replay serve: {error:#}");
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_server_s_host_is_answered() {
        assert!(allowed_host(Some("127.0.0.1:7357"), 7357));
        assert!(allowed_host(Some("localhost:7357"), 7357));
        assert!(!allowed_host(Some("evil.example:7357"), 7357));
        assert!(!allowed_host(Some("127.0.0.1:80"), 7357));
        assert!(!allowed_host(None, 7357));
    }

    #[test]
    fn posts_must_be_json_from_this_page() {
        assert!(allowed_post(
            Some("http://127.0.0.1:7357"),
            Some("application/json"),
            7357
        ));
        assert!(allowed_post(
            None,
            Some("application/json; charset=utf-8"),
            7357
        ));
        assert!(!allowed_post(
            Some("https://evil.example"),
            Some("application/json"),
            7357
        ));
        assert!(!allowed_post(
            Some("http://127.0.0.1:7357"),
            Some("text/plain"),
            7357
        ));
        assert!(!allowed_post(None, None, 7357));
    }

    #[test]
    fn the_server_listens_on_loopback_only() {
        let listener = bind(0).unwrap();
        let address = listener.local_addr().unwrap();
        assert!(address.ip().is_loopback(), "{address}");
    }

    #[test]
    fn requests_from_other_hosts_are_refused_over_http() {
        let root = tempfile::tempdir().unwrap();
        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let live = Arc::new(Live::new().unwrap());
        let root_path = root.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let _ = handle(stream.unwrap(), &root_path, port, &live);
            }
        });
        let get = |request: String| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            response
        };
        let ok = get(format!(
            "GET /api/runs HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        ));
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(!ok.to_ascii_lowercase().contains("access-control-allow"));
        let rebound = get("GET /api/runs HTTP/1.1\r\nHost: evil.example\r\n\r\n".into());
        assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");
        let traversal = get(format!(
            "GET /api/runs/../events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"
        ));
        assert!(traversal.starts_with("HTTP/1.1 404"), "{traversal}");
    }

    #[test]
    fn header_lines_are_capped() {
        let ok = format!(
            "GET /api/runs HTTP/1.1\r\nHost: 127.0.0.1:1\r\nX: {}\r\n\r\n",
            "a".repeat(100)
        );
        let request = read_request_from(&mut ok.as_bytes()).unwrap();
        assert_eq!(request.path, "/api/runs");
        assert_eq!(request.host.as_deref(), Some("127.0.0.1:1"));
        // One endless header line is refused once it passes the cap, before
        // the rest of it is read.
        let endless = std::io::Read::chain(
            &b"GET / HTTP/1.1\r\nX: "[..],
            std::io::repeat(b'a').take(10 * 1024 * 1024),
        );
        let mut reader = BufReader::new(endless);
        assert!(read_request_from(&mut reader).is_err());
        let long_first = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_LINE));
        assert!(read_request_from(&mut long_first.as_bytes()).is_err());
        // Many short lines still hit the total.
        let many = format!(
            "GET / HTTP/1.1\r\n{}\r\n",
            "X: aaaaaaaaaaaaaaaa\r\n".repeat(2000)
        );
        assert!(read_request_from(&mut many.as_bytes()).is_err());
    }

    #[test]
    fn end_records_end_the_live_stream() {
        assert!(is_end_record(
            "{\"seq\":9,\"t_ms\":1,\"audio_ms\":1,\"type\":\"meta\",\"name\":\"run-end\",\"payload\":{}}"
        ));
        assert!(!is_end_record(
            "{\"type\":\"debug\",\"name\":\"phase\",\"payload\":{\"phase\":\"run-end\"}}"
        ));
        assert_eq!(query_number("from=120&x=1", "from"), 120);
        assert_eq!(query_number("", "from"), 0);
    }

    #[test]
    fn server_sent_events_are_framed_line_by_line() {
        assert_eq!(
            sse_frame(Some("delta"), "{\"text\":\"hi\"}"),
            "event: delta\ndata: {\"text\":\"hi\"}\n\n"
        );
        assert_eq!(
            sse_frame(None, "a\nb\r\nc"),
            "data: a\ndata: b\ndata: c\n\n"
        );
        // JSON never carries a raw newline, so one record is one data line.
        let data = json!({ "text": "two\nlines" }).to_string();
        let frame = sse_frame(Some("delta"), &data);
        assert_eq!(frame.matches("data: ").count(), 1);
        assert!(frame.ends_with("\n\n"));
    }

    #[test]
    fn ranges_are_parsed_like_http_says() {
        assert_eq!(parse_range(None, 100), Ok(None));
        assert_eq!(parse_range(Some("bytes=0-0"), 100), Ok(Some((0, 0))));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), Ok(Some((10, 19))));
        assert_eq!(parse_range(Some("bytes=90-"), 100), Ok(Some((90, 99))));
        assert_eq!(
            parse_range(Some("bytes=90-500"), 100),
            Ok(Some((90, 99))),
            "clamped"
        );
        assert_eq!(
            parse_range(Some("bytes=-10"), 100),
            Ok(Some((90, 99))),
            "suffix"
        );
        assert_eq!(
            parse_range(Some("bytes=-500"), 100),
            Ok(Some((0, 99))),
            "suffix over the length"
        );
        assert_eq!(
            parse_range(Some("bytes=100-"), 100),
            Err(()),
            "past the end"
        );
        assert_eq!(parse_range(Some("bytes=-0"), 100), Err(()));
        assert_eq!(parse_range(Some("bytes=0-"), 0), Err(()), "an empty file");
        assert_eq!(
            parse_range(Some("bytes=5-2"), 100),
            Ok(None),
            "invalid: ignored"
        );
        assert_eq!(
            parse_range(Some("bytes=0-1,5-6"), 100),
            Ok(None),
            "several: whole body"
        );
        assert_eq!(parse_range(Some("items=0-1"), 100), Ok(None));
    }

    #[test]
    fn only_the_run_s_linked_recording_is_served_with_ranges() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("20261016-100000-a");
        let meeting = run.join("sandbox").join("meeting");
        std::fs::create_dir_all(&meeting).unwrap();
        let recording = tempfile::tempdir().unwrap();
        let source = recording.path().join("audio.m4a");
        std::fs::write(&source, (0u8..100).collect::<Vec<u8>>()).unwrap();
        std::fs::write(meeting.join("other.txt"), "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, meeting.join("audio.m4a")).unwrap();
        #[cfg(not(unix))]
        std::fs::copy(&source, meeting.join("audio.m4a")).unwrap();
        let (path, kind) = run_audio(&run).unwrap();
        assert_eq!(path, source.canonicalize().unwrap());
        assert_eq!(kind, "audio/mp4");

        let listener = bind(0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let live = Arc::new(Live::new().unwrap());
        let root_path = root.path().to_path_buf();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let _ = handle(stream.unwrap(), &root_path, port, &live);
            }
        });
        let get = |range: &str| {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(
                stream,
                "GET /api/runs/20261016-100000-a/audio HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{range}\r\n"
            )
            .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response
        };
        let whole = get("");
        assert!(whole.starts_with(b"HTTP/1.1 200"));
        assert!(String::from_utf8_lossy(&whole).contains("Accept-Ranges: bytes"));
        let part = get("Range: bytes=10-14\r\n");
        let text = String::from_utf8_lossy(&part);
        assert!(text.starts_with("HTTP/1.1 206"), "{text}");
        assert!(text.contains("Content-Range: bytes 10-14/100"));
        assert!(text.contains("Content-Type: audio/mp4"));
        assert!(part.ends_with(&[10, 11, 12, 13, 14]));
        let tail = get("Range: bytes=-3\r\n");
        assert!(tail.ends_with(&[97, 98, 99]));
        let past = String::from_utf8_lossy(&get("Range: bytes=200-\r\n")).to_string();
        assert!(past.starts_with("HTTP/1.1 416"), "{past}");
        assert!(past.contains("Content-Range: bytes */100"));

        std::fs::remove_file(&source).unwrap();
        assert!(run_audio(&run).unwrap_err().contains("not on this machine"));
    }
}
