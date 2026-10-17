//! `replay --serve`: the replay viewer and the local runs, over HTTP on
//! 127.0.0.1 only.
//!
//! - `/replay/…` serves the viewer (embedded from `site/replay` at build time).
//! - `/api/runs` lists the runs under the replays folder.
//! - `/api/runs/<run>/events.jsonl` and `/report.json` return a run's files.
//! - `/api/runs/<run>/live?from=<line>` streams a run's events as server-sent
//!   events, following the file while the run is still writing it.
//!
//! Read-only and local: it binds to the loopback address, answers only
//! requests addressed to it by that name (so a web page cannot reach it through
//! DNS rebinding), sends no CORS headers, serves only files it embeds or the
//! two files of a run folder directly under the replays root, and never makes
//! a network request of its own.

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub const DEFAULT_PORT: u16 = 7357;

const ASSETS: &[(&str, &str, &str)] = &[
    ("index.html", "text/html; charset=utf-8", include_str!("../../../../site/replay/index.html")),
    ("app.js", "text/javascript; charset=utf-8", include_str!("../../../../site/replay/app.js")),
    ("parser.js", "text/javascript; charset=utf-8", include_str!("../../../../site/replay/parser.js")),
    ("replay.css", "text/css; charset=utf-8", include_str!("../../../../site/replay/replay.css")),
    ("sample-data.js", "text/javascript; charset=utf-8", include_str!("../../../../site/replay/sample-data.js")),
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

/// A run folder name: what the recorder makes (`20261016-162607-m35`).
pub fn valid_run_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The folder of run `name`, only if it is a directory directly under `root`.
pub fn run_dir(root: &Path, name: &str) -> Option<PathBuf> {
    if !valid_run_name(name) {
        return None;
    }
    let root = root.canonicalize().ok()?;
    let dir = root.join(name).canonicalize().ok()?;
    (dir.parent() == Some(root.as_path()) && dir.is_dir()).then_some(dir)
}

/// Whether the `Host` header names this server (loopback, this port).
pub fn allowed_host(host: Option<&str>, port: u16) -> bool {
    matches!(host, Some(h) if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

struct Request {
    method: String,
    path: String,
    query: String,
    host: Option<String>,
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut host = None;
    let mut total = first.len();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        total += read;
        if read == 0 || line == "\r\n" || line == "\n" || total > 16 * 1024 {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("host") {
                host = Some(value.trim().to_string());
            }
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Ok(Request {
        method,
        path: path.to_string(),
        query: query.to_string(),
        host,
    })
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}

fn not_found(stream: &mut TcpStream) -> Result<()> {
    respond(stream, "404 Not Found", "text/plain", b"Not found")
}

fn first_line(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// One run's entry in `/api/runs`.
fn run_summary(dir: &Path) -> Option<Value> {
    let name = dir.file_name()?.to_str()?.to_string();
    let events = dir.join("events.jsonl");
    let meta = events.metadata().ok()?;
    let header = first_line(&events).map(|r| r["payload"].clone()).unwrap_or(Value::Null);
    let report: Option<Value> = std::fs::read_to_string(dir.join("report.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    let fresh = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age < Duration::from_secs(60));
    let status = if report.is_some() {
        "done"
    } else if fresh {
        "running"
    } else {
        "incomplete"
    };
    let count = |status: &str| {
        report
            .as_ref()
            .and_then(|r| r["criteria"].as_array())
            .map(|c| c.iter().filter(|c| c["status"] == status).count())
    };
    Some(json!({
        "name": name,
        "status": status,
        "audio_name": header["audio_name"],
        "duration_s": header["duration_s"],
        "realtime": header["realtime"],
        "started_at": header["started_at"],
        "events_bytes": meta.len(),
        "passed": count("pass"),
        "failed": count("fail"),
    }))
}

fn list_runs(root: &Path) -> Value {
    let mut runs: Vec<Value> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| e.file_name().to_str().is_some_and(valid_run_name))
        .filter_map(|e| run_summary(&e.path()))
        .collect();
    runs.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    json!({ "runs": runs })
}

fn is_end_record(line: &str) -> bool {
    line.contains("\"type\":\"meta\"")
        && (line.contains("\"name\":\"run-end\"") || line.contains("\"name\":\"run-error\""))
}

/// Streams `events.jsonl` from line `from` as server-sent events, following
/// the file until the run ends (or the viewer goes away).
fn live(stream: &mut TcpStream, dir: &Path, from: usize) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    )?;
    let path = dir.join("events.jsonl");
    let mut offset = 0u64;
    let mut pending = String::new();
    let mut line_no = 0usize;
    let mut last_send = Instant::now();
    loop {
        let mut file = std::fs::File::open(&path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = String::new();
        let read = file.read_to_string(&mut chunk)?;
        offset += read as u64;
        pending.push_str(&chunk);
        let mut ended = false;
        let mut out = String::new();
        while let Some(newline) = pending.find('\n') {
            let line: String = pending.drain(..=newline).collect();
            let line = line.trim_end();
            if line.is_empty() {
                continue;
            }
            if line_no >= from {
                out.push_str("data: ");
                out.push_str(line);
                out.push_str("\n\n");
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
            stream.write_all(b"event: end\ndata: {}\n\n")?;
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

fn handle(mut stream: TcpStream, root: &Path, port: u16) -> Result<()> {
    let request = read_request(&mut stream)?;
    if !allowed_host(request.host.as_deref(), port) {
        return respond(&mut stream, "403 Forbidden", "text/plain", b"Forbidden");
    }
    if request.method != "GET" {
        return respond(&mut stream, "405 Method Not Allowed", "text/plain", b"Read-only");
    }
    let path = request.path.as_str();
    if path == "/" || path == "/replay" {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: /replay/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    if let Some(asset) = path.strip_prefix("/replay/") {
        let asset = if asset.is_empty() { "index.html" } else { asset };
        return match ASSETS.iter().find(|(name, _, _)| *name == asset) {
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
        let body = serde_json::to_vec(&list_runs(root))?;
        return respond(&mut stream, "200 OK", "application/json", &body);
    }
    if let Some(rest) = path.strip_prefix("/api/runs/") {
        let Some((name, file)) = rest.split_once('/') else {
            return not_found(&mut stream);
        };
        let Some(dir) = run_dir(root, name) else {
            return not_found(&mut stream);
        };
        return match file {
            "events.jsonl" | "report.json" => match std::fs::read(dir.join(file)) {
                Ok(body) => respond(
                    &mut stream,
                    "200 OK",
                    if file.ends_with(".json") {
                        "application/json"
                    } else {
                        "application/x-ndjson"
                    },
                    &body,
                ),
                Err(_) => not_found(&mut stream),
            },
            "live" => live(&mut stream, &dir, query_number(&request.query, "from")),
            _ => not_found(&mut stream),
        };
    }
    not_found(&mut stream)
}

/// Serves the viewer and the runs under `root` until killed.
pub fn serve(root: PathBuf, port: u16, open: bool) -> Result<()> {
    std::fs::create_dir_all(&root)?;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| anyhow!("could not listen on 127.0.0.1:{port}: {e}"))?;
    let url = format!("http://127.0.0.1:{port}/replay/");
    println!("Replay viewer: {url}");
    println!("Runs: {} (read-only, this machine only; Ctrl-C to stop)", root.display());
    if open {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let root = root.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &root, port);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_names_and_folders_stay_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("20261016-162607-m35")).unwrap();
        std::fs::write(root.path().join("file.txt"), "x").unwrap();
        assert!(run_dir(root.path(), "20261016-162607-m35").is_some());
        for bad in ["", "..", "../etc", "a/b", ".hidden", "file.txt", "missing", "%2e%2e"] {
            assert!(run_dir(root.path(), bad).is_none(), "{bad}");
        }
        let outside = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
            assert!(run_dir(root.path(), "link").is_none(), "a symlink out of the root");
        }
    }

    #[test]
    fn only_this_server_s_host_is_answered() {
        assert!(allowed_host(Some("127.0.0.1:7357"), 7357));
        assert!(allowed_host(Some("localhost:7357"), 7357));
        assert!(!allowed_host(Some("evil.example:7357"), 7357));
        assert!(!allowed_host(Some("127.0.0.1:80"), 7357));
        assert!(!allowed_host(None, 7357));
    }

    #[test]
    fn runs_are_listed_with_their_status() {
        let root = tempfile::tempdir().unwrap();
        let done = root.path().join("20261016-100000-a");
        std::fs::create_dir(&done).unwrap();
        std::fs::write(
            done.join("events.jsonl"),
            "{\"seq\":0,\"t_ms\":0,\"audio_ms\":0,\"type\":\"meta\",\"name\":\"run-start\",\"payload\":{\"audio_name\":\"audio.mp4\",\"duration_s\":60}}\n",
        )
        .unwrap();
        std::fs::write(
            done.join("report.json"),
            "{\"criteria\":[{\"status\":\"pass\"},{\"status\":\"fail\"},{\"status\":\"pass\"}]}",
        )
        .unwrap();
        let running = root.path().join("20261016-110000-b");
        std::fs::create_dir(&running).unwrap();
        std::fs::write(running.join("events.jsonl"), "").unwrap();
        std::fs::create_dir(root.path().join("no-events")).unwrap();
        let list = list_runs(root.path());
        let runs = list["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0]["name"], "20261016-110000-b");
        assert_eq!(runs[0]["status"], "running");
        assert_eq!(runs[1]["status"], "done");
        assert_eq!(runs[1]["passed"], 2);
        assert_eq!(runs[1]["failed"], 1);
        assert_eq!(runs[1]["duration_s"], 60);
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
}
