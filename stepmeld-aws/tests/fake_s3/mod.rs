//! A stand-in S3 for the tests: a small HTTP server on a local port
//! that answers the calls `S3Objects` makes (GET, conditional PUT,
//! DELETE, ListObjectsV2 in pages), so the tests cover the requests the
//! SDK actually sends. After veokit's, much smaller.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct State {
    /// (bucket, key) -> (body, etag)
    pub objects: BTreeMap<(String, String), (Vec<u8>, String)>,
    ids: u64,
    /// Keys per listing page.
    pub page: usize,
}

pub struct FakeS3 {
    pub endpoint: String,
    pub state: Arc<Mutex<State>>,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn answer(stream: &mut TcpStream, status: &str, headers: &[(&str, String)], body: &[u8]) {
    let mut head = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\n", body.len());
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn error(stream: &mut TcpStream, status: &str, code: &str) {
    let body = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{code}</Message></Error>");
    answer(stream, status, &[("content-type", "application/xml".into())], body.as_bytes());
}

fn serve(mut stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
        let mut headers = BTreeMap::new();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let len: usize = headers.get("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
        let mut body = vec![0; len];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        let query: BTreeMap<String, String> = query
            .split('&')
            .filter(|q| !q.is_empty())
            .map(|q| q.split_once('=').map_or((decode(q), String::new()), |(k, v)| (decode(k), decode(v))))
            .collect();
        let path = path.trim_start_matches('/');
        let (bucket, key) = path.split_once('/').map_or((path.to_string(), String::new()), |(b, k)| (b.to_string(), decode(k)));
        let mut s = state.lock().unwrap();
        match (method.as_str(), key.is_empty()) {
            ("GET", true) => {
                let prefix = query.get("prefix").cloned().unwrap_or_default();
                let after = query.get("continuation-token").cloned().unwrap_or_default();
                let keys: Vec<String> = s.objects.keys().filter(|(b, k)| *b == bucket && k.starts_with(&prefix) && k.as_str() > after.as_str()).map(|(_, k)| k.clone()).collect();
                let page = if s.page == 0 { keys.len() } else { s.page };
                let shown: Vec<&String> = keys.iter().take(page).collect();
                let truncated = keys.len() > shown.len();
                let mut xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult><Name>{bucket}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><IsTruncated>{truncated}</IsTruncated>",
                    escape(&prefix),
                    shown.len()
                );
                for k in &shown {
                    xml.push_str(&format!("<Contents><Key>{}</Key><Size>0</Size></Contents>", escape(k)));
                }
                if truncated {
                    xml.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", escape(shown.last().unwrap())));
                }
                xml.push_str("</ListBucketResult>");
                answer(&mut stream, "200 OK", &[("content-type", "application/xml".into())], xml.as_bytes());
            }
            ("GET", false) => match s.objects.get(&(bucket, key)) {
                Some((b, etag)) => {
                    let (b, etag) = (b.clone(), etag.clone());
                    answer(&mut stream, "200 OK", &[("etag", etag)], &b);
                }
                None => error(&mut stream, "404 Not Found", "NoSuchKey"),
            },
            ("PUT", false) => {
                let at = (bucket, key);
                let held = s.objects.get(&at).map(|(_, e)| e.clone());
                let ok = match (headers.get("if-none-match"), headers.get("if-match")) {
                    (Some(_), _) => held.is_none(),
                    (_, Some(m)) => held.as_deref() == Some(m.as_str()),
                    _ => true,
                };
                if !ok {
                    error(&mut stream, "412 Precondition Failed", "PreconditionFailed");
                } else {
                    s.ids += 1;
                    let etag = format!("\"{}\"", s.ids);
                    s.objects.insert(at, (body, etag.clone()));
                    answer(&mut stream, "200 OK", &[("etag", etag)], b"");
                }
            }
            ("DELETE", false) => {
                s.objects.remove(&(bucket, key));
                answer(&mut stream, "204 No Content", &[], b"");
            }
            _ => error(&mut stream, "400 Bad Request", "NotImplemented"),
        }
    }
}

impl FakeS3 {
    pub fn start() -> FakeS3 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                std::thread::spawn(move || serve(stream, state));
            }
        });
        FakeS3 { endpoint, state }
    }
}
