//! Just enough HTTP/1.1 for the spike: the server reads one request head and
//! answers with a JSON body and `Connection: close`; the LocalAPI client sends
//! one `GET` and reads the whole response. TASK-018 replaces this with a real
//! HTTP and WebSocket stack.

use std::io::{self, Read, Write};

/// Largest request head the spike server reads.
pub const MAX_REQUEST_HEAD: usize = 16 * 1024;

/// The request line of an HTTP/1.x request. Headers are read but not kept: the
/// caller's identity comes only from the TCP peer address, never from headers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub target: String,
    pub version: String,
}

#[derive(Debug)]
pub enum HeadError {
    /// The connection ended before a complete head.
    Closed,
    TooLarge,
    Malformed(&'static str),
    Io(io::Error),
}

impl std::fmt::Display for HeadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("connection closed before a complete request head"),
            Self::TooLarge => write!(f, "request head over {MAX_REQUEST_HEAD} bytes"),
            Self::Malformed(why) => write!(f, "malformed request: {why}"),
            Self::Io(error) => write!(f, "reading the request: {error}"),
        }
    }
}

/// Reads up to the blank line that ends the request head. Bytes after it (a
/// body) are left unread or discarded. Each read blocks as `reader` does; the
/// server passes a reader that fails once its deadline passes.
pub fn read_request_head<R: Read>(reader: &mut R) -> Result<RequestHead, HeadError> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = find(&buf, b"\r\n\r\n") {
            return parse_request_head(&buf[..end]);
        }
        if buf.len() > MAX_REQUEST_HEAD {
            return Err(HeadError::TooLarge);
        }
        match reader.read(&mut chunk) {
            Ok(0) => return Err(HeadError::Closed),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(HeadError::Io(error)),
        }
    }
}

fn parse_request_head(head: &[u8]) -> Result<RequestHead, HeadError> {
    let head = std::str::from_utf8(head).map_err(|_| HeadError::Malformed("not UTF-8"))?;
    let line = head.split("\r\n").next().unwrap_or_default();
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(HeadError::Malformed("request line"));
    };
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(HeadError::Malformed("method"));
    }
    if !target.starts_with('/') {
        return Err(HeadError::Malformed("target"));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(HeadError::Malformed("version"));
    }
    Ok(RequestHead {
        method: method.to_owned(),
        target: target.to_owned(),
        version: version.to_owned(),
    })
}

/// Writes a complete response with a JSON body and closes the exchange.
pub fn write_json_response<W: Write>(writer: &mut W, status: u16, body: &[u8]) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        reason_phrase(status),
        body.len()
    );
    writer.write_all(head.as_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        500 => "Internal Server Error",
        _ => "",
    }
}

/// A response read by the LocalAPI client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body as text for error messages, trimmed and cut short.
    pub fn body_text(&self) -> String {
        let text = String::from_utf8_lossy(&self.body);
        let text = text.trim();
        match text.char_indices().nth(200) {
            Some((cut, _)) => format!("{}...", &text[..cut]),
            None => text.to_owned(),
        }
    }
}

/// Parses a complete response read until the server closed the connection.
/// Handles `Content-Length`, `Transfer-Encoding: chunked`, and neither.
pub fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let end = find(raw, b"\r\n\r\n").ok_or("no end of response head")?;
    let head = std::str::from_utf8(&raw[..end]).map_err(|_| "response head is not UTF-8")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(format!("not an HTTP/1 response: {status_line:?}"));
    }
    let status = parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line: {status_line:?}"))?;
    let mut headers = Vec::new();
    for line in lines {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("bad header line: {line:?}"))?;
        headers.push((key.trim().to_owned(), value.trim().to_owned()));
    }
    let mut response = Response {
        status,
        headers,
        body: Vec::new(),
    };
    let rest = &raw[end + 4..];
    let chunked = response
        .header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    response.body = if chunked {
        decode_chunked(rest)?
    } else if let Some(length) = response.header("content-length") {
        let length: usize = length
            .parse()
            .map_err(|_| format!("bad Content-Length {length:?}"))?;
        rest.get(..length)
            .ok_or_else(|| format!("body shorter than Content-Length {length}"))?
            .to_vec()
    } else {
        rest.to_vec()
    };
    Ok(response)
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line_end = find(data, b"\r\n").ok_or("chunk size line not terminated")?;
        let size_line = std::str::from_utf8(&data[..line_end]).map_err(|_| "chunk size")?;
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("bad chunk size {size_hex:?}"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        let chunk = data.get(..size).ok_or("chunk shorter than its size")?;
        body.extend_from_slice(chunk);
        data = data
            .get(size..)
            .and_then(|rest| rest.strip_prefix(b"\r\n"))
            .ok_or("chunk not followed by CRLF")?;
    }
}

/// Standard base64 with padding, for the LocalAPI's Basic authorization.
pub fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for group in input.chunks(3) {
        let bytes = [
            group[0],
            *group.get(1).unwrap_or(&0),
            *group.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= group.len() {
                out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Percent-encodes a query parameter value (everything but RFC 3986 unreserved).
pub fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_request_line_and_ignores_headers() {
        let raw = b"GET /v1/agents HTTP/1.1\r\nHost: 100.101.102.104:7413\r\nX-Tailscale-User: mallory@example.com\r\n\r\nbody";
        let head = read_request_head(&mut &raw[..]).unwrap();
        assert_eq!(
            head,
            RequestHead {
                method: "GET".into(),
                target: "/v1/agents".into(),
                version: "HTTP/1.1".into(),
            }
        );
    }

    #[test]
    fn reads_a_head_split_across_reads() {
        struct Trickle<'a>(&'a [u8]);
        impl Read for Trickle<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let Some((first, rest)) = self.0.split_first() else {
                    return Ok(0);
                };
                buf[0] = *first;
                self.0 = rest;
                Ok(1)
            }
        }
        let head = read_request_head(&mut Trickle(b"POST /v1/ask HTTP/1.0\r\n\r\n")).unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.version, "HTTP/1.0");
    }

    #[test]
    fn rejects_bad_heads() {
        for raw in [
            &b"GET /v1/agents\r\n\r\n"[..],
            b"get /v1/agents HTTP/1.1\r\n\r\n",
            b"GET v1/agents HTTP/1.1\r\n\r\n",
            b"GET /v1/agents HTTP/2\r\n\r\n",
            b"GET /a b HTTP/1.1\r\n\r\n",
            b"\xff\xfe /x HTTP/1.1\r\n\r\n",
        ] {
            assert!(
                matches!(
                    read_request_head(&mut &raw[..]),
                    Err(HeadError::Malformed(_))
                ),
                "{:?}",
                String::from_utf8_lossy(raw)
            );
        }
        assert!(matches!(
            read_request_head(&mut &b"GET / HTTP/1.1\r\n"[..]),
            Err(HeadError::Closed)
        ));
        let huge = vec![b'a'; MAX_REQUEST_HEAD + 2048];
        assert!(matches!(
            read_request_head(&mut &huge[..]),
            Err(HeadError::TooLarge)
        ));
    }

    #[test]
    fn writes_a_complete_json_response() {
        let mut out = Vec::new();
        write_json_response(&mut out, 403, b"{\"error\":{}}").unwrap();
        let response = parse_response(&out).unwrap();
        assert_eq!(response.status, 403);
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.header("content-length"), Some("12"));
        assert_eq!(response.header("connection"), Some("close"));
        assert_eq!(response.body, b"{\"error\":{}}");
        assert!(out.starts_with(b"HTTP/1.1 403 Forbidden\r\n"));
    }

    #[test]
    fn parses_content_length_chunked_and_close_delimited_bodies() {
        let fixed = parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra").unwrap();
        assert_eq!(fixed.body, b"{}");

        let chunked = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\na;ext=1\r\n:12345678}\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(chunked.body, b"{\"a\":12345678}");

        let closed =
            parse_response(b"HTTP/1.0 404 Not Found\r\n\r\nno match for IP:port\n").unwrap();
        assert_eq!(closed.status, 404);
        assert_eq!(closed.body_text(), "no match for IP:port");
    }

    #[test]
    fn rejects_bad_responses() {
        for raw in [
            &b"HTTP/1.1 200 OK\r\n"[..],
            b"SSH-2.0-OpenSSH\r\n\r\n",
            b"HTTP/1.1 abc OK\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab",
        ] {
            assert!(
                parse_response(raw).is_err(),
                "{:?}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            (":0123456789abcdef0123", "OjAxMjM0NTY3ODlhYmNkZWYwMTIz"),
        ] {
            assert_eq!(base64(input.as_bytes()), expected, "{input}");
        }
    }

    #[test]
    fn encodes_addresses_for_the_query() {
        assert_eq!(encode_query_value("100.64.0.1:7413"), "100.64.0.1%3A7413");
        assert_eq!(
            encode_query_value("[fd7a:115c:a1e0::1]:7413"),
            "%5Bfd7a%3A115c%3Aa1e0%3A%3A1%5D%3A7413"
        );
    }
}
