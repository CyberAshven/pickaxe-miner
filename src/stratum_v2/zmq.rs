//! #### PR #42: ZMQ block notices
//! What: a minimal ZMTP 3.0 subscriber (NULL security, no dependency) to a
//! node's `zmqpubhashblock`: the greeting, a READY as a SUB socket, a
//! subscription to `hashblock`, then `[topic, hash, sequence]` messages.
//! Each new block wakes the ASIC server's node worker at once instead of at
//! its next 250 ms poll; while notices flow, the node's tip is asked every
//! 2 s instead of at every wake. Polling stays as the safety net.
//! Why: a new block reached devices up to a poll late, with a tip call to
//! the node four times a second.
//! Look here if: new blocks reach devices late although the dashboard says
//! ZMQ is connected, or ZMQ never connects.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// How often the node's tip is asked while notices flow.
pub const CONNECTED_POLL: Duration = Duration::from_secs(2);

/// The largest frame taken; block notices are 32 bytes.
const MAX_FRAME: u64 = 64 * 1024;

/// How long a publisher has for its greeting and READY.
const HANDSHAKE: Duration = Duration::from_secs(5);

/// What the subscription tells the node worker and the dashboard.
#[derive(Debug, Default)]
pub struct BlockNotices {
    fresh: AtomicBool,
    connected: AtomicBool,
    blocks: AtomicU64,
    problem: Mutex<Option<String>>,
}

impl BlockNotices {
    /// Whether a block came since the last call.
    pub fn take_fresh(&self) -> bool {
        self.fresh.swap(false, Ordering::AcqRel)
    }

    /// Whether the publisher is connected and subscribed.
    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// One line for the dashboard.
    pub fn summary(&self) -> String {
        let blocks = self.blocks.load(Ordering::Relaxed);
        if self.connected() {
            let unit = if blocks == 1 { "block" } else { "blocks" };
            return format!("connected · {blocks} {unit}");
        }
        match self.problem.lock().ok().and_then(|problem| problem.clone()) {
            Some(problem) => format!("not connected ({problem}); polling the node"),
            None => "connecting".into(),
        }
    }

    fn set_problem(&self, problem: Option<String>) {
        if let Ok(mut slot) = self.problem.lock() {
            *slot = problem;
        }
    }
}

/// Whether the node worker asks the node for its tip now: at every wake
/// without notices, and every `CONNECTED_POLL` while they flow.
pub fn should_ask_tip(notices: Option<&BlockNotices>, since_asked: Duration) -> bool {
    !notices.is_some_and(BlockNotices::connected) || since_asked >= CONNECTED_POLL
}

/// The `HOST:PORT` of a `tcp://HOST:PORT` endpoint.
pub fn address_of(url: &str) -> Result<String, String> {
    let address = url
        .trim()
        .strip_prefix("tcp://")
        .ok_or("a ZMQ endpoint looks like tcp://127.0.0.1:28332")?
        .trim_end_matches('/');
    let port = address.rsplit_once(':').map(|(_, port)| port);
    if port
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .is_none()
    {
        return Err("a ZMQ endpoint needs its port, such as tcp://127.0.0.1:28332".into());
    }
    Ok(address.to_owned())
}

/// Subscribes to `url`'s block notices on a thread until `stop`: each new
/// block marks the notices fresh and wakes `wake`. A lost connection is
/// tried again after 1 s, doubling to 30 s.
pub fn subscribe(
    url: &str,
    wake: SyncSender<()>,
    stop: Arc<AtomicBool>,
) -> Result<Arc<BlockNotices>, String> {
    let address = address_of(url)?;
    let notices = Arc::new(BlockNotices::default());
    let shared = notices.clone();
    thread::spawn(move || {
        let mut backoff = Duration::from_secs(1);
        while !stop.load(Ordering::Relaxed) {
            let result = listen(&address, &shared, &wake, &stop);
            if shared.connected.swap(false, Ordering::AcqRel) {
                backoff = Duration::from_secs(1);
            }
            if let Err(problem) = result {
                shared.set_problem(Some(problem));
            }
            let until = Instant::now() + backoff;
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(50));
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
    Ok(notices)
}

/// One connection: the handshake, then notices until it fails or `stop`.
fn listen(
    address: &str,
    notices: &BlockNotices,
    wake: &SyncSender<()>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut stream = address
        .to_socket_addrs()
        .map_err(|_| format!("{address} does not resolve"))?
        .take(4)
        .find_map(|target| TcpStream::connect_timeout(&target, Duration::from_secs(2)).ok())
        .ok_or_else(|| format!("nothing answers at {address}"))?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    handshake(&mut stream, stop)?;
    notices.set_problem(None);
    notices.connected.store(true, Ordering::Release);
    loop {
        let message = read_message(&mut stream, stop, None)?;
        if message.first().map(Vec::as_slice) == Some(b"hashblock".as_slice()) {
            notices.blocks.fetch_add(1, Ordering::Relaxed);
            notices.fresh.store(true, Ordering::Release);
            // A full slot already wakes the worker.
            let _ = wake.try_send(());
        }
    }
}

/// Our greeting: ZMTP 3.0, the NULL mechanism, as a client.
fn greeting() -> [u8; 64] {
    let mut greeting = [0u8; 64];
    greeting[0] = 0xff;
    greeting[9] = 0x7f;
    greeting[10] = 3;
    greeting[12..16].copy_from_slice(b"NULL");
    greeting
}

/// The publisher's greeting must be ZMTP 3 or later with NULL security.
fn check_greeting(greeting: &[u8; 64]) -> Result<(), String> {
    if greeting[0] != 0xff || greeting[9] != 0x7f {
        return Err("not a ZMQ publisher (bad greeting)".into());
    }
    if greeting[10] < 3 {
        return Err(format!("ZMTP {} is too old", greeting[10]));
    }
    if &greeting[12..16] != b"NULL" || greeting[16..32].iter().any(|byte| *byte != 0) {
        return Err("the publisher wants security other than NULL".into());
    }
    Ok(())
}

/// A frame: flags (0x01 more, 0x02 long size, 0x04 command), size, body.
fn frame(flags: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 9);
    match u8::try_from(body.len()) {
        Ok(size) => out.extend_from_slice(&[flags, size]),
        Err(_) => {
            out.push(flags | 0x02);
            out.extend_from_slice(&(body.len() as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(body);
    out
}

/// READY as a SUB socket.
fn ready() -> Vec<u8> {
    let mut body = vec![5];
    body.extend_from_slice(b"READY");
    body.push(11);
    body.extend_from_slice(b"Socket-Type");
    body.extend_from_slice(&3u32.to_be_bytes());
    body.extend_from_slice(b"SUB");
    frame(0x04, &body)
}

fn handshake(stream: &mut TcpStream, stop: &AtomicBool) -> Result<(), String> {
    let deadline = Some(Instant::now() + HANDSHAKE);
    stream
        .write_all(&greeting())
        .map_err(|error| format!("write: {error}"))?;
    let mut theirs = [0u8; 64];
    read_full(stream, &mut theirs, stop, deadline)?;
    check_greeting(&theirs)?;
    stream
        .write_all(&ready())
        .map_err(|error| format!("write: {error}"))?;
    let (flags, body) = read_frame(stream, stop, deadline)?;
    let name_len = usize::from(body.first().copied().unwrap_or(0));
    let name = body.get(1..1 + name_len).unwrap_or_default();
    if flags & 0x04 == 0 || name != b"READY" {
        let reason = if name == b"ERROR" {
            let reason = body.get(1 + name_len + 1..).unwrap_or_default();
            format!("the publisher refused: {}", String::from_utf8_lossy(reason))
        } else {
            "the publisher sent no READY".into()
        };
        return Err(reason);
    }
    // ZMTP 3.0 subscribes with a message: 0x01 and the topic.
    stream
        .write_all(&frame(0x00, b"\x01hashblock"))
        .map_err(|error| format!("write: {error}"))
}

/// One message's parts, skipping commands between messages.
fn read_message(
    stream: &mut impl Read,
    stop: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<Vec<Vec<u8>>, String> {
    let mut parts = Vec::new();
    loop {
        let (flags, body) = read_frame(stream, stop, deadline)?;
        if flags & 0x04 != 0 {
            if parts.is_empty() {
                continue;
            }
            return Err("a command inside a message".into());
        }
        parts.push(body);
        if flags & 0x01 == 0 {
            return Ok(parts);
        }
        if parts.len() >= 8 {
            return Err("a message of over 8 parts".into());
        }
    }
}

fn read_frame(
    stream: &mut impl Read,
    stop: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<(u8, Vec<u8>), String> {
    let mut flags = [0u8; 1];
    read_full(stream, &mut flags, stop, deadline)?;
    let flags = flags[0];
    if flags & 0xf8 != 0 {
        return Err(format!("a frame with flags {flags:#04x}"));
    }
    let size = if flags & 0x02 != 0 {
        let mut size = [0u8; 8];
        read_full(stream, &mut size, stop, deadline)?;
        u64::from_be_bytes(size)
    } else {
        let mut size = [0u8; 1];
        read_full(stream, &mut size, stop, deadline)?;
        u64::from(size[0])
    };
    if size > MAX_FRAME {
        return Err(format!("a frame of {size} bytes"));
    }
    let mut body = vec![0u8; size as usize];
    read_full(stream, &mut body, stop, deadline)?;
    Ok((flags, body))
}

/// Fills `buf` across read timeouts, until `stop` or `deadline`.
fn read_full(
    stream: &mut impl Read,
    buf: &mut [u8],
    stop: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let mut filled = 0;
    while filled < buf.len() {
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err("the publisher did not answer in time".into());
        }
        match stream.read(&mut buf[filled..]) {
            Ok(0) => return Err("the publisher closed the connection".into()),
            Ok(read) => filled += read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(format!("read: {error}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc;

    /// A publisher's side of the handshake; returns once subscribed.
    fn publisher_handshake(stream: &mut TcpStream) {
        let mut greeting_in = [0u8; 64];
        stream.read_exact(&mut greeting_in).unwrap();
        check_greeting(&greeting_in).unwrap();
        let mut ours = greeting();
        ours[32] = 1;
        stream.write_all(&ours).unwrap();
        let stop = AtomicBool::new(false);
        let (flags, body) = read_frame(stream, &stop, None).unwrap();
        assert_eq!(flags, 0x04);
        assert!(body.starts_with(b"\x05READY\x0bSocket-Type\x00\x00\x00\x03SUB"));
        let mut ready = vec![5];
        ready.extend_from_slice(b"READY\x0bSocket-Type\x00\x00\x00\x03PUB");
        stream.write_all(&frame(0x04, &ready)).unwrap();
        let (flags, body) = read_frame(stream, &stop, None).unwrap();
        assert_eq!((flags, body.as_slice()), (0, b"\x01hashblock".as_slice()));
    }

    // #### PR #42
    // What: a block notice from a ZMQ publisher wakes the node worker at
    // once (well before its next 250 ms poll), marks the notices fresh and
    // counts the block; while connected, the tip is asked every 2 s.
    // Look here if: subscribe, handshake or read_message change.
    #[test]
    fn a_fake_publisher_wakes_the_worker_at_once() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (send_block, block) = mpsc::channel::<()>();
        let (sent_at, sent) = mpsc::channel::<Instant>();
        let publisher = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            publisher_handshake(&mut stream);
            block.recv().unwrap();
            let mut message = frame(0x01, b"hashblock");
            message.extend(frame(0x01, &[0xab; 32]));
            message.extend(frame(0x00, &7u32.to_le_bytes()));
            stream.write_all(&message).unwrap();
            sent_at.send(Instant::now()).unwrap();
            // Held open until the test ends.
            let _ = block.recv();
        });
        let (wake, woken) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let notices = subscribe(&format!("tcp://127.0.0.1:{port}"), wake, stop.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !notices.connected() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(notices.connected(), "{}", notices.summary());
        assert!(!should_ask_tip(Some(&notices), Duration::from_millis(300)));
        assert!(should_ask_tip(Some(&notices), CONNECTED_POLL));
        send_block.send(()).unwrap();
        woken.recv_timeout(Duration::from_secs(2)).unwrap();
        let elapsed = sent.recv().unwrap().elapsed();
        assert!(elapsed < Duration::from_millis(250), "{elapsed:?}");
        assert!(notices.take_fresh());
        assert!(!notices.take_fresh());
        assert_eq!(notices.summary(), "connected · 1 block");
        stop.store(true, Ordering::Relaxed);
        drop(send_block);
        publisher.join().unwrap();
    }

    // #### PR #42
    // What: something that is not a ZMQ publisher is left with its reason
    // on the dashboard, wakes nothing, and the node worker keeps polling.
    // Look here if: check_greeting or should_ask_tip change.
    #[test]
    fn a_bad_greeting_is_ignored_with_a_reason_and_polling_continues() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut greeting_in = [0u8; 64];
            stream.read_exact(&mut greeting_in).unwrap();
            stream.write_all(&[b'H'; 64]).unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let (wake, woken) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let notices = subscribe(&format!("tcp://127.0.0.1:{port}"), wake, stop.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !notices.summary().contains("bad greeting") && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            notices.summary(),
            "not connected (not a ZMQ publisher (bad greeting)); polling the node"
        );
        assert!(!notices.connected());
        assert!(woken.try_recv().is_err());
        assert!(should_ask_tip(Some(&notices), Duration::ZERO));
        assert!(should_ask_tip(None, Duration::ZERO));
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        assert!(address_of("http://127.0.0.1:28332").is_err());
        assert!(address_of("tcp://127.0.0.1").is_err());
        assert_eq!(
            address_of("tcp://127.0.0.1:28332/").unwrap(),
            "127.0.0.1:28332"
        );
    }
}
