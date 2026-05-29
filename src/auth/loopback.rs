//! Ephemeral loopback HTTP listener that captures the OAuth redirect.
//!
//! Per RFC 8252 (OAuth for native apps), the CLI binds an ephemeral port on
//! `127.0.0.1`, uses it as the `redirect_uri`, and waits for the browser to
//! deliver the authorization `code` (and `state`) via a single GET request.
//! This is a minimal HTTP/1.1 responder — just enough to read the request
//! line, validate `state`, and show the user a "you can close this tab" page.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::ActualError;

/// The authorization code + state returned on the redirect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRedirect {
    pub code: String,
    pub state: String,
}

/// A bound loopback listener awaiting the OAuth redirect.
pub struct LoopbackServer {
    listener: TcpListener,
    addr: SocketAddr,
}

impl LoopbackServer {
    /// Bind an ephemeral port on `127.0.0.1`.
    pub async fn bind() -> Result<Self, ActualError> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| ActualError::ApiError(format!("Failed to bind loopback listener: {e}")))?;
        let addr = listener
            .local_addr()
            .map_err(|e| ActualError::ApiError(format!("Failed to read loopback address: {e}")))?;
        Ok(Self { listener, addr })
    }

    /// The `redirect_uri` to register with the authorization request.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.addr.port())
    }

    /// The bound port (mainly for tests/diagnostics).
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Wait for the browser redirect, bounded by `timeout`. Validates that the
    /// returned `state` matches `expected_state` (CSRF protection).
    pub async fn wait_for_code(
        self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<AuthRedirect, ActualError> {
        tokio::time::timeout(timeout, self.accept_until_callback(expected_state))
            .await
            .map_err(|_| {
                ActualError::ApiError(
                    "Timed out waiting for the browser redirect. \
                     Re-run `actual login` and complete sign-in in the browser."
                        .to_string(),
                )
            })?
    }

    /// Accept connections until one carries our callback (or a stray request,
    /// e.g. `/favicon.ico`, which we 404 and keep waiting past).
    async fn accept_until_callback(
        &self,
        expected_state: &str,
    ) -> Result<AuthRedirect, ActualError> {
        loop {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|e| ActualError::ApiError(format!("Loopback accept failed: {e}")))?;

            let request_line = read_request_line(&mut stream).await?;
            let target = request_target(&request_line);
            let params = parse_query(query_string(target));

            if let Some(err) = params
                .iter()
                .find(|(k, _)| k == "error")
                .map(|(_, v)| v.clone())
            {
                let desc = param(&params, "error_description").unwrap_or_default();
                write_html(
                    &mut stream,
                    "400 Bad Request",
                    "Sign-in failed",
                    "Authorization failed. You can close this tab and return to the terminal.",
                )
                .await;
                return Err(ActualError::ApiError(format!(
                    "Authorization failed: {err}{}",
                    if desc.is_empty() {
                        String::new()
                    } else {
                        format!(" — {desc}")
                    }
                )));
            }

            match (param(&params, "code"), param(&params, "state")) {
                (Some(code), Some(state)) => {
                    if state != expected_state {
                        write_html(
                            &mut stream,
                            "400 Bad Request",
                            "Sign-in failed",
                            "Security check failed. Close this tab and try again.",
                        )
                        .await;
                        return Err(ActualError::ApiError(
                            "OAuth state mismatch — possible CSRF; aborting login.".to_string(),
                        ));
                    }
                    write_html(
                        &mut stream,
                        "200 OK",
                        "Signed in",
                        "You're signed in to Actual AI. You can close this tab and return to the terminal.",
                    )
                    .await;
                    return Ok(AuthRedirect { code, state });
                }
                _ => {
                    // Stray request (favicon, prefetch, etc.) — 404 and keep waiting.
                    write_html(&mut stream, "404 Not Found", "Not found", "").await;
                    continue;
                }
            }
        }
    }
}

/// Read and return the HTTP request line (first line, sans CRLF).
async fn read_request_line<S>(stream: &mut S) -> Result<String, ActualError>
where
    S: AsyncReadExt + Unpin,
{
    const CAP: usize = 16 * 1024;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream
            .read(&mut chunk)
            .await
            .map_err(|e| ActualError::ApiError(format!("Loopback read failed: {e}")))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_crlf(&buf) {
            return Ok(String::from_utf8_lossy(&buf[..pos]).into_owned());
        }
        if buf.len() > CAP {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf)
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}

/// Extract the request target (path+query) from a request line like
/// `GET /callback?code=x&state=y HTTP/1.1`.
fn request_target(request_line: &str) -> &str {
    request_line.split_whitespace().nth(1).unwrap_or("")
}

/// Return the query-string portion of a target (after `?`), or "".
fn query_string(target: &str) -> &str {
    match target.split_once('?') {
        Some((_, q)) => q,
        None => "",
    }
}

/// Parse a query string into decoded key/value pairs.
fn parse_query(query: &str) -> Vec<(String, String)> {
    if query.is_empty() {
        return Vec::new();
    }
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

fn param(params: &[(String, String)], key: &str) -> Option<String> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
}

/// Minimal percent-decoding for query values (`%XX` → byte). A literal `+` is
/// left as-is (OAuth query params are percent-encoded, not form-encoded).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Write a minimal HTML response and close the connection.
async fn write_html<S>(stream: &mut S, status: &str, title: &str, message: &str)
where
    S: AsyncWriteExt + Unpin,
{
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{title}</title></head>\
         <body style=\"font-family:-apple-system,system-ui,sans-serif;text-align:center;padding-top:4rem;color:#222\">\
         <h1>{title}</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    #[test]
    fn test_request_target() {
        assert_eq!(
            request_target("GET /callback?code=a&state=b HTTP/1.1"),
            "/callback?code=a&state=b"
        );
        assert_eq!(request_target("GET / HTTP/1.1"), "/");
        assert_eq!(request_target("garbage"), "");
    }

    #[test]
    fn test_query_string() {
        assert_eq!(query_string("/callback?code=a&state=b"), "code=a&state=b");
        assert_eq!(query_string("/callback"), "");
    }

    #[test]
    fn test_parse_query() {
        let q = parse_query("code=abc&state=xyz");
        assert_eq!(param(&q, "code"), Some("abc".to_string()));
        assert_eq!(param(&q, "state"), Some("xyz".to_string()));
        assert_eq!(param(&q, "missing"), None);
    }

    #[test]
    fn test_parse_query_empty() {
        assert!(parse_query("").is_empty());
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("plain"), "plain");
        // Malformed trailing percent is left intact.
        assert_eq!(percent_decode("bad%2"), "bad%2");
        assert_eq!(percent_decode("base64url-_value"), "base64url-_value");
    }

    #[tokio::test]
    async fn test_wait_for_code_happy_path() {
        let server = LoopbackServer::bind().await.unwrap();
        let port = server.port();
        let handle =
            tokio::spawn(async move { server.wait_for_code("xyz", Duration::from_secs(5)).await });

        // Simulate the browser redirect.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"GET /callback?code=the-code&state=xyz HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        // Drain the response so the server can finish writing.
        let mut resp = Vec::new();
        let _ = stream.read_to_end(&mut resp).await;

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.code, "the-code");
        assert_eq!(result.state, "xyz");
        assert!(
            String::from_utf8_lossy(&resp).contains("Signed in"),
            "browser should see the success page"
        );
    }

    #[tokio::test]
    async fn test_wait_for_code_state_mismatch() {
        let server = LoopbackServer::bind().await.unwrap();
        let port = server.port();
        let handle = tokio::spawn(async move {
            server
                .wait_for_code("expected-state", Duration::from_secs(5))
                .await
        });

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"GET /callback?code=c&state=WRONG HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = stream.read_to_end(&mut resp).await;

        let err = handle.await.unwrap().unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("state mismatch")),
            "expected state-mismatch error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_wait_for_code_error_param() {
        let server = LoopbackServer::bind().await.unwrap();
        let port = server.port();
        let handle =
            tokio::spawn(async move { server.wait_for_code("s", Duration::from_secs(5)).await });

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"GET /callback?error=access_denied&error_description=nope HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = stream.read_to_end(&mut resp).await;

        let err = handle.await.unwrap().unwrap_err();
        assert!(
            matches!(err, ActualError::ApiError(ref m) if m.contains("access_denied")),
            "expected authorization-failed error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_wait_for_code_times_out() {
        let server = LoopbackServer::bind().await.unwrap();
        // No client connects; should time out quickly.
        let result = server.wait_for_code("s", Duration::from_millis(150)).await;
        assert!(
            matches!(result, Err(ActualError::ApiError(ref m)) if m.contains("Timed out")),
            "expected timeout error, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_redirect_uri_shape() {
        let server = LoopbackServer::bind().await.unwrap();
        let uri = server.redirect_uri();
        assert!(uri.starts_with("http://127.0.0.1:"));
        assert!(uri.ends_with("/callback"));
    }

    #[test]
    fn test_parse_query_bare_key() {
        // A param with no `=` exercises the `None` split branch.
        let q = parse_query("flag&code=c");
        assert_eq!(param(&q, "flag"), Some(String::new()));
        assert_eq!(param(&q, "code"), Some("c".to_string()));
    }

    #[test]
    fn test_percent_decode_invalid_hex_passthrough() {
        // %ZZ: to_digit returns None → bytes pass through unchanged.
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[tokio::test]
    async fn test_wait_for_code_skips_stray_request_then_succeeds() {
        let server = LoopbackServer::bind().await.unwrap();
        let port = server.port();
        let handle =
            tokio::spawn(async move { server.wait_for_code("s", Duration::from_secs(5)).await });

        // A stray request with no code/state (e.g. favicon) → 404, loop continues.
        let mut stray = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stray
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut sresp = Vec::new();
        let _ = stray.read_to_end(&mut sresp).await;
        assert!(String::from_utf8_lossy(&sresp).contains("404 Not Found"));

        // Then the real callback.
        let mut ok = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        ok.write_all(b"GET /callback?code=c&state=s HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut oresp = Vec::new();
        let _ = ok.read_to_end(&mut oresp).await;

        let redirect = handle.await.unwrap().unwrap();
        assert_eq!(redirect.code, "c");
    }

    #[tokio::test]
    async fn test_wait_for_code_error_without_description() {
        let server = LoopbackServer::bind().await.unwrap();
        let port = server.port();
        let handle =
            tokio::spawn(async move { server.wait_for_code("s", Duration::from_secs(5)).await });
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        // error param but NO error_description → exercises the empty-desc branch.
        stream
            .write_all(b"GET /callback?error=access_denied HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        let _ = stream.read_to_end(&mut resp).await;
        let err = handle.await.unwrap().unwrap_err();
        assert!(matches!(err, ActualError::ApiError(ref m) if m.contains("access_denied")));
    }

    #[tokio::test]
    async fn test_wait_for_code_eof_without_crlf_uses_fallback_parse() {
        // Bytes with no CRLF, then EOF → read hits n==0 and the fallback
        // first-line parse runs, still extracting the code.
        let server = LoopbackServer::bind().await.unwrap();
        let port = server.port();
        let handle =
            tokio::spawn(
                async move { server.wait_for_code("s", Duration::from_millis(500)).await },
            );
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        stream
            .write_all(b"GET /callback?code=c&state=s")
            .await
            .unwrap(); // no CRLF
        stream.shutdown().await.unwrap(); // EOF
        let mut resp = Vec::new();
        let _ = stream.read_to_end(&mut resp).await;
        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "fallback parse should extract code: {result:?}"
        );
        assert_eq!(result.unwrap().code, "c");
    }

    #[tokio::test]
    async fn test_wait_for_code_oversized_without_crlf_hits_cap() {
        // >16 KiB with no CRLF → the CAP break path; no valid callback → 404,
        // then the accept loop waits and times out.
        let server = LoopbackServer::bind().await.unwrap();
        let port = server.port();
        let handle =
            tokio::spawn(
                async move { server.wait_for_code("s", Duration::from_millis(400)).await },
            );
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let big = vec![b'x'; 17 * 1024];
        stream.write_all(&big).await.unwrap();
        let _ = stream.shutdown().await;
        let mut resp = Vec::new();
        let _ = stream.read_to_end(&mut resp).await;
        let result = handle.await.unwrap();
        assert!(
            matches!(result, Err(ActualError::ApiError(ref m)) if m.contains("Timed out")),
            "got: {result:?}"
        );
    }
}
