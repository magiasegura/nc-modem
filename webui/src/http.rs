//! Минимальный HTTP/1.1-сервер на std.
//!
//! Ровно столько, сколько нужно локальному веб-интерфейсу: GET/POST, разбор
//! Content-Length, Basic-аутентификация, отдача статики из бинаря.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Больше запросов одновременно локальному UI не нужно, а роутер слабый.
const MAX_CONNECTIONS: usize = 16;
/// Защита от медленных/зависших клиентов.
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Тела запросов у нас крошечные; всё сверх — мусор или атака.
const MAX_BODY: usize = 64 * 1024;
const MAX_HEADER_LINE: usize = 8 * 1024;

pub struct Request {
    pub method: String,
    /// Путь без строки запроса — роутинг по нему не должен зависеть от `?…`.
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub extra_headers: Vec<(String, String)>,
}

impl Response {
    pub fn json(status: u16, body: String) -> Response {
        Response {
            status,
            content_type: "application/json; charset=utf-8".to_string(),
            body: body.into_bytes(),
            extra_headers: Vec::new(),
        }
    }

    pub fn html(body: &str) -> Response {
        Response {
            status: 200,
            content_type: "text/html; charset=utf-8".to_string(),
            body: body.as_bytes().to_vec(),
            extra_headers: Vec::new(),
        }
    }

    pub fn asset(content_type: &str, body: &str) -> Response {
        Response {
            status: 200,
            content_type: content_type.to_string(),
            body: body.as_bytes().to_vec(),
            extra_headers: Vec::new(),
        }
    }

    pub fn text(status: u16, body: &str) -> Response {
        Response {
            status,
            content_type: "text/plain; charset=utf-8".to_string(),
            body: body.as_bytes().to_vec(),
            extra_headers: Vec::new(),
        }
    }

    fn unauthorized() -> Response {
        Response {
            status: 401,
            content_type: "text/plain; charset=utf-8".to_string(),
            body: b"authentication required".to_vec(),
            extra_headers: vec![(
                "WWW-Authenticate".to_string(),
                "Basic realm=\"modemui\"".to_string(),
            )],
        }
    }
}

fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Unknown",
    }
}

/// Запустить сервер. `auth` — `Some((user, pass))` включает Basic-аутентификацию.
pub fn serve<F>(
    listener: TcpListener,
    auth: Option<(String, String)>,
    handler: F,
) -> std::io::Result<()>
where
    F: Fn(&Request) -> Response + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    let auth = Arc::new(auth);
    let live = Arc::new(AtomicUsize::new(0));

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("modemui: соединение отклонено: {}", e);
                continue;
            }
        };

        if live.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
            let mut s = stream;
            let _ = write_response(&mut s, &Response::text(503, "too many connections"));
            continue;
        }

        let handler = Arc::clone(&handler);
        let auth = Arc::clone(&auth);
        let live = Arc::clone(&live);
        live.fetch_add(1, Ordering::SeqCst);

        std::thread::spawn(move || {
            let mut stream = stream;
            if let Err(e) = handle_connection(&mut stream, &auth, &*handler) {
                // Обрыв соединения — норма, шуметь в лог не о чем.
                if e.kind() != std::io::ErrorKind::UnexpectedEof {
                    eprintln!("modemui: {}", e);
                }
            }
            live.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

fn handle_connection<F>(
    stream: &mut TcpStream,
    auth: &Option<(String, String)>,
    handler: &F,
) -> std::io::Result<()>
where
    F: Fn(&Request) -> Response,
{
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let req = match read_request(stream)? {
        Some(r) => r,
        None => return Ok(()),
    };

    let resp = if !check_auth(&req, auth) {
        Response::unauthorized()
    } else {
        handler(&req)
    };

    write_response(stream, &resp)
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut line = String::new();
    if read_line_limited(&mut reader, &mut line)? == 0 {
        return Ok(None);
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    if method.is_empty() {
        return Ok(None);
    }

    let path = match target.split_once('?') {
        Some((p, _query)) => p.to_string(),
        None => target,
    };

    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        let n = read_line_limited(&mut reader, &mut h)?;
        if n == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    if len > MAX_BODY {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request body too large",
        ));
    }

    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }

    Ok(Some(Request {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

/// `read_line`, но с потолком — иначе клиент может съесть всю память роутера.
fn read_line_limited(
    reader: &mut BufReader<TcpStream>,
    out: &mut String,
) -> std::io::Result<usize> {
    let mut total = 0;
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(total),
            Ok(_) => {
                total += 1;
                out.push(byte[0] as char);
                if byte[0] == b'\n' {
                    return Ok(total);
                }
                if total > MAX_HEADER_LINE {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "header line too long",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
    }
}

fn write_response(stream: &mut TcpStream, resp: &Response) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\
         X-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n",
        resp.status,
        status_text(resp.status),
        resp.content_type,
        resp.body.len()
    );
    for (k, v) in &resp.extra_headers {
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    head.push_str("\r\n");

    stream.write_all(head.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()
}

// ---------------------------------------------------------------------------
// Basic-аутентификация
// ---------------------------------------------------------------------------

fn check_auth(req: &Request, auth: &Option<(String, String)>) -> bool {
    let (user, pass) = match auth {
        None => return true,
        Some(p) => p,
    };

    let header = match req.headers.get("authorization") {
        Some(h) => h,
        None => return false,
    };
    let encoded = match header
        .strip_prefix("Basic ")
        .or_else(|| header.strip_prefix("basic "))
    {
        Some(e) => e.trim(),
        None => return false,
    };
    let decoded = match base64_decode(encoded) {
        Some(d) => d,
        None => return false,
    };
    let text = String::from_utf8_lossy(&decoded);
    let (u, p) = match text.split_once(':') {
        Some(v) => v,
        None => return false,
    };

    // Сравнение за постоянное время: пароль не должен утекать по таймингу.
    constant_time_eq(u.as_bytes(), user.as_bytes())
        & constant_time_eq(p.as_bytes(), pass.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let bytes: Vec<u8> = bytes.into_iter().take_while(|&b| b != b'=').collect();

    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;

    for b in bytes {
        let v = val(b)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_auth(header: Option<&str>) -> Request {
        let mut headers = HashMap::new();
        if let Some(h) = header {
            headers.insert("authorization".to_string(), h.to_string());
        }
        Request {
            method: "GET".to_string(),
            path: "/".to_string(),

            headers,
            body: String::new(),
        }
    }

    #[test]
    fn base64_roundtrip_known_vectors() {
        assert_eq!(base64_decode("YWJj").unwrap(), b"abc");
        assert_eq!(base64_decode("YQ==").unwrap(), b"a");
        assert_eq!(base64_decode("YWI=").unwrap(), b"ab");
        assert_eq!(base64_decode("dXNlcjpwYXNz").unwrap(), b"user:pass");
        assert_eq!(base64_decode("").unwrap(), b"");
    }

    #[test]
    fn base64_rejects_garbage() {
        assert!(base64_decode("!!!!").is_none());
    }

    #[test]
    fn auth_disabled_allows_everything() {
        assert!(check_auth(&req_with_auth(None), &None));
    }

    #[test]
    fn auth_requires_correct_credentials() {
        let cfg = Some(("user".to_string(), "pass".to_string()));
        // dXNlcjpwYXNz == "user:pass"
        assert!(check_auth(&req_with_auth(Some("Basic dXNlcjpwYXNz")), &cfg));
        // dXNlcjp3cm9uZw== == "user:wrong"
        assert!(!check_auth(
            &req_with_auth(Some("Basic dXNlcjp3cm9uZw==")),
            &cfg
        ));
        assert!(!check_auth(&req_with_auth(None), &cfg));
        assert!(!check_auth(&req_with_auth(Some("Bearer x")), &cfg));
    }

    #[test]
    fn constant_time_eq_behaves_like_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
