//! #### PR #53: HTTP GET for P2Pool's releases (GitHub's API and its
//! downloads, which redirect to a file host) and for the node's local API.
//! HTTPS goes through rustls with the system's certificates (`tls`), as for
//! Fulcrum servers and https nodes; no HTTP library is added for this.
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

/// Redirects followed before giving up.
const MAX_REDIRECTS: usize = 6;
/// Room for the status line and headers above the body limit.
const HEADER_ROOM: usize = 64 << 10;

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

    /// The body when the status is 2xx; otherwise an error naming it.
    pub fn ok(self, what: &str) -> Result<Vec<u8>, String> {
        if (200..300).contains(&self.status) {
            Ok(self.body)
        } else {
            Err(format!("{what}: HTTP {}", self.status))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Url {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> Result<Url, String> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("{url}: only http and https URLs"));
    };
    let split = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(split);
    if authority.contains('@') {
        return Err(format!("{url}: no logins in download URLs"));
    }
    let path = match path {
        "" => "/".to_owned(),
        path if path.starts_with('?') => format!("/{path}"),
        path => path.to_owned(),
    };
    let default = if secure { 443 } else { 80 };
    let port_of = |port: &str| {
        port.parse::<u16>()
            .map_err(|_| format!("{url}: bad port {port}"))
    };
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed
            .split_once(']')
            .ok_or_else(|| format!("{url}: unclosed IPv6 bracket"))?;
        match after.strip_prefix(':') {
            Some(port) => (host, port_of(port)?),
            None if after.is_empty() => (host, default),
            None => return Err(format!("{url}: bad host")),
        }
    } else if authority.matches(':').count() > 1 {
        return Err(format!("{url}: IPv6 hosts need brackets"));
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, port_of(port)?),
            None => (authority, default),
        }
    };
    if host.is_empty() {
        return Err(format!("{url}: no host"));
    }
    Ok(Url {
        secure,
        host: host.to_owned(),
        port,
        path,
    })
}

/// `location` (absolute, or a path on `base`'s server) as a URL.
fn resolve(base: &str, location: &str) -> Result<String, String> {
    if location.starts_with("https://") || location.starts_with("http://") {
        return Ok(location.to_owned());
    }
    let base = parse_url(base)?;
    if !location.starts_with('/') {
        return Err(format!("unsupported redirect to {location}"));
    }
    let host = if base.host.contains(':') {
        format!("[{}]", base.host)
    } else {
        base.host
    };
    Ok(format!(
        "{}://{host}:{}{location}",
        if base.secure { "https" } else { "http" },
        base.port
    ))
}

/// GETs `url`, following redirects, with a body of at most `limit` bytes.
/// An https URL never redirects to plain http.
pub fn get(url: &str, accept: &str, limit: usize, timeout: Duration) -> Result<Response, String> {
    let mut url = url.to_owned();
    for _ in 0..=MAX_REDIRECTS {
        let response = get_once(&url, accept, limit, timeout)?;
        if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let location = response
            .header("location")
            .ok_or_else(|| format!("{url}: redirect without a location"))?;
        let next = resolve(&url, location)?;
        if url.starts_with("https://") && !next.starts_with("https://") {
            return Err(format!("{url}: refusing a redirect from https to {next}"));
        }
        url = next;
    }
    Err(format!("{url}: too many redirects"))
}

fn get_once(url: &str, accept: &str, limit: usize, timeout: Duration) -> Result<Response, String> {
    request_once(url, "GET", accept, None, limit, Budget::Each(timeout))
}

/// POST a bounded preparation request. No redirect can forward its identity
/// or token to another service. Authentication remains the API's responsibility.
/// `budget` is the whole request's: the name lookup, the connection, TLS,
/// the request and every read of the reply end within it.
pub fn post_json(
    url: &str,
    body: &[u8],
    limit: usize,
    budget: Duration,
) -> Result<Response, String> {
    validate_preparation_url(url)?;
    if body.len() > 8192 {
        return Err("P2Pool preparation request is too large".into());
    }
    request_once(
        url,
        "POST",
        "application/json",
        Some(body),
        limit,
        Budget::Until(Instant::now() + budget),
    )
    .map_err(|_| "P2Pool preparation connection or response failed".to_owned())
}

// #### PR #65: one deadline for a preparation request
// What: a P2Pool preparation request has one deadline for all of it: the
// name lookup (on its own thread, abandoned when the deadline passes), the
// connection, the TLS handshake, the request and every read of the reply.
// Downloads and the node's local API keep their wait per step.
// Why: a slow or silent helper held the miner's offer for 3 s per read, and
// the name lookup had no limit at all.
// Look here if: a preparation takes longer than its budget, or lookups fail
// with "still running".
#[derive(Clone, Copy)]
enum Budget {
    /// Each step (the connection, a read, a write) may take this long.
    Each(Duration),
    /// Everything ends by then.
    Until(Instant),
}

impl Budget {
    /// How long the next step may wait; nothing once the deadline passed.
    fn step(self) -> Option<Duration> {
        match self {
            Self::Each(timeout) => Some(timeout),
            Self::Until(deadline) => Some(deadline.saturating_duration_since(Instant::now()))
                .filter(|left| !left.is_zero()),
        }
    }
}

/// A stream whose reads and writes wait at most for the budget's next step.
struct Bounded {
    stream: TcpStream,
    budget: Budget,
}

impl Bounded {
    fn arm(&self, read: bool) -> io::Result<()> {
        if let Budget::Until(_) = self.budget {
            let left = self
                .budget
                .step()
                .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
            if read {
                self.stream.set_read_timeout(Some(left))?;
            } else {
                self.stream.set_write_timeout(Some(left))?;
            }
        }
        Ok(())
    }
}

impl Read for Bounded {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.arm(true)?;
        self.stream.read(buffer)
    }
}

impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.arm(false)?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Lookups that may still run past their deadline; beyond this many, new
/// ones are refused.
const MAX_LOOKUPS: usize = 4;
static LOOKUPS: AtomicUsize = AtomicUsize::new(0);

/// `host`'s addresses; under a deadline the lookup runs on its own thread.
fn addresses(host: &str, port: u16, budget: Budget) -> Result<Vec<SocketAddr>, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    match budget {
        Budget::Each(_) => (host, port)
            .to_socket_addrs()
            .map(Iterator::collect)
            .map_err(|error| format!("{host}: {error}")),
        Budget::Until(deadline) => {
            let name = host.to_owned();
            lookup_within(deadline, move || {
                (name.as_str(), port)
                    .to_socket_addrs()
                    .map(Iterator::collect)
            })
            .map_err(|error| format!("{host}: {error}"))
        }
    }
}

/// Runs `lookup` on its own thread, waiting for it until `deadline`.
fn lookup_within<F>(deadline: Instant, lookup: F) -> Result<Vec<SocketAddr>, String>
where
    F: FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    if LOOKUPS.fetch_add(1, Ordering::SeqCst) >= MAX_LOOKUPS {
        LOOKUPS.fetch_sub(1, Ordering::SeqCst);
        return Err("earlier name lookups are still running".into());
    }
    let (send, receive) = mpsc::sync_channel(1);
    let started = thread::Builder::new()
        .name("pickaxe-lookup".into())
        .spawn(move || {
            let found = lookup();
            LOOKUPS.fetch_sub(1, Ordering::SeqCst);
            let _ = send.send(found);
        });
    if started.is_err() {
        LOOKUPS.fetch_sub(1, Ordering::SeqCst);
        return Err("cannot start the name lookup".into());
    }
    match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(found) => found.map_err(|error| error.to_string()),
        Err(_) => Err("the name lookup did not finish in time".into()),
    }
}

/// Remote preparation carries job tokens and identities; require authenticated
/// TLS. Plain HTTP is only for a helper on this computer, never a public node.
pub fn validate_preparation_url(url: &str) -> Result<(), String> {
    crate::config::validate_p2pool_preparation_url(url)
}

fn request_once(
    url: &str,
    method: &str,
    accept: &str,
    body: Option<&[u8]>,
    limit: usize,
    budget: Budget,
) -> Result<Response, String> {
    let target = parse_url(url)?;
    let stream = addresses(&target.host, target.port, budget)?
        .into_iter()
        .find_map(|address| TcpStream::connect_timeout(&address, budget.step()?).ok())
        .ok_or_else(|| format!("nothing answers at {}:{}", target.host, target.port))?;
    if let Budget::Each(timeout) = budget {
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
    }
    let stream = Bounded { stream, budget };
    let host = if target.host.contains(':') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    let host = match (target.secure, target.port) {
        (true, 443) | (false, 80) => host,
        (_, port) => format!("{host}:{port}"),
    };
    let content = body.map_or_else(String::new, |body| {
        format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        )
    });
    let request = format!(
        "{method} {} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: pickaxe-miner/{}\r\nAccept: {accept}\r\n\
         Accept-Encoding: identity\r\n{content}Connection: close\r\n\r\n",
        target.path,
        env!("CARGO_PKG_VERSION"),
    );
    let mut request = request.into_bytes();
    if let Some(body) = body {
        request.extend(body);
    }
    let raw = if target.secure {
        let mut tls = crate::tls::connect(&target.host, stream)?;
        tls.write_all(&request)
            .map_err(|error| format!("{url}: {error}"))?;
        read_capped(&mut tls, limit + HEADER_ROOM)
    } else {
        let mut stream = stream;
        stream
            .write_all(&request)
            .map_err(|error| format!("{url}: {error}"))?;
        read_capped(&mut stream, limit + HEADER_ROOM)
    }
    .map_err(|error| format!("{url}: {error}"))?;
    parse_response(&raw, limit).map_err(|error| format!("{url}: {error}"))
}

/// Reads until the server closes, refusing more than `cap` bytes. A TLS
/// server that closes without a close_notify still ends the response.
fn read_capped(reader: &mut impl Read, cap: usize) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    let mut buffer = vec![0u8; 64 << 10];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(raw),
            Ok(read) => {
                raw.extend_from_slice(&buffer[..read]);
                if raw.len() > cap {
                    return Err(format!("the response is larger than {cap} bytes"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof && !raw.is_empty() => {
                return Ok(raw)
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Splits a whole HTTP/1.x response into status, headers and body, decoding
/// a chunked body and checking a declared length.
pub(crate) fn parse_response(raw: &[u8], limit: usize) -> Result<Response, String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("no end of headers")?;
    let head = std::str::from_utf8(&raw[..split]).map_err(|_| "headers are not text")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    if !parts
        .next()
        .is_some_and(|version| version.starts_with("HTTP/1."))
    {
        return Err(format!("not an HTTP response: {status_line}"));
    }
    let status = parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("no status in {status_line}"))?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let mut response = Response {
        status,
        headers,
        body: Vec::new(),
    };
    let body = &raw[split + 4..];
    response.body = if response
        .header("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        dechunk(body)?
    } else if let Some(length) = response.header("content-length") {
        let length: usize = length
            .parse()
            .map_err(|_| format!("bad content-length {length}"))?;
        if body.len() < length {
            return Err(format!(
                "the body ended after {} of {length} bytes",
                body.len()
            ));
        }
        body[..length].to_vec()
    } else {
        body.to_vec()
    };
    if response.body.len() > limit {
        return Err(format!("the body is larger than {limit} bytes"));
    }
    Ok(response)
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = body
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("a chunk size line has no end")?;
        let size_text = std::str::from_utf8(&body[..line_end]).map_err(|_| "bad chunk size")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| format!("bad chunk size {size_text}"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err("the body ended inside a chunk".into());
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves each of `answers` to one connection in turn on 127.0.0.1 and
    /// returns its port; each answer may name `{port}` for redirects.
    pub(crate) fn serve(answers: Vec<Vec<u8>>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let answer = String::from_utf8_lossy(&answer)
                    .replace("{port}", &port.to_string())
                    .into_bytes();
                let _ = stream.write_all(&answer);
            }
        });
        port
    }

    #[test]
    fn urls_parse_with_default_ports_paths_and_brackets() {
        assert_eq!(
            parse_url("https://api.github.com/repos/a/b/releases?per_page=30").unwrap(),
            Url {
                secure: true,
                host: "api.github.com".into(),
                port: 443,
                path: "/repos/a/b/releases?per_page=30".into(),
            }
        );
        let local = parse_url("http://127.0.0.1:46860/health").unwrap();
        assert_eq!(
            (local.secure, local.port, local.path.as_str()),
            (false, 46860, "/health")
        );
        assert_eq!(parse_url("http://[::1]:8080").unwrap().host, "::1");
        assert_eq!(parse_url("https://[::1]/x").unwrap().port, 443);
        assert!(parse_url("http://::1:80/").is_err());
        assert!(parse_url("ftp://example.com").is_err());
        assert!(parse_url("https://user:pass@example.com/").is_err());
        assert_eq!(
            resolve("https://a.example:443/x", "/y?z").unwrap(),
            "https://a.example:443/y?z"
        );
    }

    #[test]
    fn chunked_and_sized_bodies_decode_and_short_or_large_bodies_fail() {
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;x=y\r\n world\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked, 100).unwrap().body, b"hello world");
        let sized = b"HTTP/1.1 404 Not Found\r\nContent-Length: 3\r\n\r\nabcEXTRA";
        let response = parse_response(sized, 100).unwrap();
        assert_eq!(
            (response.status, response.body.as_slice()),
            (404, &b"abc"[..])
        );
        assert!(response.ok("thing").unwrap_err().contains("HTTP 404"));
        assert!(
            parse_response(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nabc", 100)
                .unwrap_err()
                .contains("3 of 9")
        );
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n\r\n0123456789", 5)
            .unwrap_err()
            .contains("larger than 5"));
        assert!(parse_response(b"SSH-2.0\r\n\r\n", 5).is_err());
    }

    #[test]
    fn get_follows_a_redirect_to_the_file() {
        let port = serve(vec![
            b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/file\r\nContent-Length: 0\r\n\r\n"
                .to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndata".to_vec(),
        ]);
        let response = get(
            &format!("http://127.0.0.1:{port}/asset"),
            "*/*",
            100,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(response.body, b"data");
    }
}

#[cfg(test)]
mod preparation_tests {
    use super::*;

    #[test]
    fn preparation_transport_requires_tls_or_literal_loopback_and_never_logs_login() {
        assert!(validate_preparation_url("https://pool.example/jd_prepare").is_ok());
        assert!(validate_preparation_url("http://127.0.0.1:8000/jd_prepare").is_ok());
        assert!(validate_preparation_url("http://[::1]:8000/jd_prepare").is_ok());
        assert!(validate_preparation_url("http://pool.example/jd_prepare").is_err());
        assert!(validate_preparation_url("http://127.0.0.1.example/jd_prepare").is_err());
        let error =
            validate_preparation_url("https://owner:secret@pool.example/jd_prepare").unwrap_err();
        assert!(!error.contains("secret"));
        assert!(validate_preparation_url("https://pool.example/jd_prepare\r\nOther: bad").is_err());
    }

    #[test]
    fn preparation_never_follows_redirects_or_sends_an_oversized_body() {
        let port=tests::serve(vec![b"HTTP/1.1 307 Redirect\r\nLocation: https://other.example/\r\nContent-Length: 0\r\n\r\n".to_vec()]);
        let response = post_json(
            &format!("http://127.0.0.1:{port}/jd_prepare"),
            b"{}",
            100,
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(response.status, 307);
        assert!(post_json(
            "http://127.0.0.1:1/jd_prepare",
            &vec![0; 8193],
            100,
            Duration::from_secs(1)
        )
        .unwrap_err()
        .contains("too large"));
    }

    /// A helper on 127.0.0.1 that reads one request, then either says
    /// nothing or sends a byte every 50 ms, for 3 s.
    fn slow_helper(trickle: bool) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buffer = [0u8; 1024];
            let _ = stream.read(&mut buffer);
            let until = Instant::now() + Duration::from_secs(3);
            if trickle {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n");
            }
            while Instant::now() < until {
                if trickle && stream.write_all(b"X").is_err() {
                    return;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        port
    }

    // #### PR #65
    // What: a helper that answers nothing, or a byte at a time, fails the
    // preparation at its one deadline, not a wait per read.
    // Look here if: Budget or Bounded changes.
    #[test]
    fn a_silent_or_trickling_helper_fails_at_the_one_deadline() {
        for trickle in [false, true] {
            let port = slow_helper(trickle);
            let start = Instant::now();
            let error = post_json(
                &format!("http://127.0.0.1:{port}/jd_prepare"),
                b"{}",
                100_000,
                Duration::from_millis(400),
            )
            .unwrap_err();
            let took = start.elapsed();
            assert!(error.contains("failed"), "{error}");
            assert!(
                took >= Duration::from_millis(350) && took < Duration::from_millis(2500),
                "trickle={trickle}: {took:?}"
            );
        }
    }

    // #### PR #65
    // What: a name lookup that hangs is abandoned at the request's deadline;
    // a literal address needs no lookup.
    // Look here if: lookup_within or addresses changes.
    #[test]
    fn a_hung_name_lookup_ends_at_the_deadline() {
        let start = Instant::now();
        let error = lookup_within(start + Duration::from_millis(150), || {
            thread::sleep(Duration::from_secs(2));
            Ok(Vec::new())
        })
        .unwrap_err();
        assert!(error.contains("in time"), "{error}");
        assert!(start.elapsed() < Duration::from_millis(1500));
        let found = lookup_within(Instant::now() + Duration::from_secs(1), || {
            Ok(vec!["127.0.0.1:9".parse().unwrap()])
        })
        .unwrap();
        assert_eq!(found, vec!["127.0.0.1:9".parse::<SocketAddr>().unwrap()]);
        assert_eq!(
            addresses("::1", 80, Budget::Until(Instant::now())).unwrap(),
            vec!["[::1]:80".parse::<SocketAddr>().unwrap()]
        );
    }
}
