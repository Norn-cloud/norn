//! #1710: `norn-olapd`'s bounded HTTP/1.1 transport. Every read and write
//! has an absolute deadline, never a per-read timeout a trickling peer could
//! renew forever: the head must arrive within [`HEAD_DEADLINE`] of the
//! accept and within [`MAX_HEAD_BYTES`], read no further than that budget;
//! a body earns its time by the bytes that arrived ([`TRANSFER_GRACE`] plus
//! those at [`MIN_READ_BYTES_PER_SEC`]), never by the length it declares;
//! an answer gets [`write_budget`] for its length. A body's buffer grows
//! only as its bytes arrive and never past its declared length.

use std::io::{BufRead as _, BufReader, Read as _};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use norn_cell::analytics::EngineRefusal;

use super::refusal;

/// Longest accepted request head, in bytes.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Longest accepted body (a sync batch), in bytes.
pub(super) const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// #1710: the whole request head must arrive within this of the accept,
/// however slowly its bytes trickle in.
pub(super) const HEAD_DEADLINE: Duration = Duration::from_secs(10);
/// #1710: the time any transfer is granted besides its bytes.
const TRANSFER_GRACE: Duration = Duration::from_secs(10);
/// #1710: the slowest rate a body may arrive at: by the time it has sent
/// `n` bytes, it has had [`TRANSFER_GRACE`] plus `n` at this rate.
const MIN_READ_BYTES_PER_SEC: u64 = 1024 * 1024;
/// #1710: the slowest rate an answer may be taken at, once the call was
/// admitted: low enough for a slow legitimate download (20 MiB over 4 Mbit/s).
const MIN_WRITE_BYTES_PER_SEC: u64 = 128 * 1024;
/// The most a body is read in one step, and the least its buffer grows by.
const BODY_STEP: usize = 64 * 1024;
/// The longest any one read or write may wait for the peer.
const IDLE: Duration = Duration::from_secs(30);
/// What is read and discarded after an early refusal, so closing with the
/// refused body unread does not reset the connection before the answer.
const MAX_DRAIN_BYTES: u64 = 64 * 1024;
/// How long that discarding may take.
const DRAIN_DEADLINE: Duration = Duration::from_secs(2);

/// A request's head: everything before its body.
pub(super) struct Head {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) authorization: Option<String>,
    pub(super) length: usize,
}

/// A request as received: its method, path and whole body (its credential
/// was verified from its head before the body was read).
pub(super) struct Request {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) body: Vec<u8>,
}

/// #1710: a connection whose every read and write must finish by one
/// absolute deadline. Each read or write waits at most what is left of it
/// (and at most [`IDLE`]), so a peer trickling one byte at a time cannot
/// hold a worker past the deadline, as it could a per-read timeout.
pub(super) struct Deadlined<'a> {
    stream: &'a TcpStream,
    deadline: Instant,
}

impl<'a> Deadlined<'a> {
    pub(super) fn new(stream: &'a TcpStream, budget: Duration) -> std::io::Result<Self> {
        Ok(Self {
            stream,
            deadline: deadline_in(budget)?,
        })
    }

    /// Grant the transfers from now on `budget` more, from now.
    fn renew(&mut self, budget: Duration) -> std::io::Result<()> {
        self.deadline = deadline_in(budget)?;
        Ok(())
    }

    /// What one read or write may still wait: the rest of the deadline,
    /// at most [`IDLE`]; none once the deadline has passed.
    fn left(&self) -> std::io::Result<Duration> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the connection's deadline passed",
            ));
        }
        Ok(left.min(IDLE))
    }
}

impl std::io::Read for Deadlined<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stream.set_read_timeout(Some(self.left()?))?;
        let mut stream = self.stream;
        stream.read(buf)
    }
}

impl std::io::Write for Deadlined<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.stream.set_write_timeout(Some(self.left()?))?;
        let mut stream = self.stream;
        stream.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut stream = self.stream;
        stream.flush()
    }
}

fn deadline_in(budget: Duration) -> std::io::Result<Instant> {
    Instant::now().checked_add(budget).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the host clock cannot bound this transfer",
        )
    })
}

/// What sending an answer of `bytes` is granted: [`TRANSFER_GRACE`] plus
/// its length at [`MIN_WRITE_BYTES_PER_SEC`].
pub(super) fn write_budget(bytes: impl TryInto<u64>) -> Duration {
    budget(TRANSFER_GRACE, bytes, MIN_WRITE_BYTES_PER_SEC)
}

/// `grace` plus `bytes` at `rate` bytes a second.
fn budget(grace: Duration, bytes: impl TryInto<u64>, rate: u64) -> Duration {
    let bytes = bytes.try_into().unwrap_or(u64::MAX);
    let millis = u128::from(bytes)
        .saturating_mul(1000)
        .checked_div(u128::from(rate))
        .unwrap_or(u128::MAX);
    grace.saturating_add(Duration::from_millis(
        u64::try_from(millis).unwrap_or(u64::MAX),
    ))
}

/// After an early refusal, stop sending and read (and drop) a bounded
/// remainder of what the peer sent, within [`DRAIN_DEADLINE`], so the
/// close does not reset the connection before the peer reads the answer.
pub(super) fn drain(reader: &mut BufReader<Deadlined<'_>>) {
    let _stopped = reader.get_ref().stream.shutdown(std::net::Shutdown::Write);
    if reader.get_mut().renew(DRAIN_DEADLINE).is_ok() {
        let _drained = std::io::copy(&mut reader.take(MAX_DRAIN_BYTES), &mut std::io::sink());
    }
}

/// #1710: read a request's head within [`HEAD_DEADLINE`] of the accept,
/// never reading past [`MAX_HEAD_BYTES`]: each line may take only what is
/// left of the head's budget, so an over-long line is cut off as it arrives.
pub(super) fn read_head(
    reader: &mut BufReader<Deadlined<'_>>,
) -> Result<Head, (u16, EngineRefusal)> {
    let invalid = |detail: String| (400, refusal("analytics.invalid", &detail));
    let mut line = String::new();
    let mut head = 0_usize;
    let mut next = |reader: &mut BufReader<Deadlined<'_>>, line: &mut String| {
        line.clear();
        // One byte past the bound, so an over-long head is told apart.
        let left = MAX_HEAD_BYTES.saturating_sub(head).saturating_add(1);
        let read = reader
            .by_ref()
            .take(u64::try_from(left).unwrap_or(u64::MAX))
            .read_line(line)
            .map_err(|e| invalid(format!("unreadable request: {e}")))?;
        head = head.saturating_add(read);
        if read == 0 || head > MAX_HEAD_BYTES {
            return Err(invalid(
                "the request head is missing or too long".to_owned(),
            ));
        }
        Ok(())
    };
    next(reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(invalid("the request line is malformed".to_owned()));
    };
    let (method, path) = (
        method.to_owned(),
        target.split('?').next().unwrap_or_default().to_owned(),
    );
    let (mut length, mut authorization) = (None::<usize>, None);
    loop {
        next(reader, &mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                // A repeated or signed length makes the body boundary
                // ambiguous between this reader and any proxy before it.
                let value = value.trim();
                if length.is_some() {
                    return Err(invalid("content-length is repeated".to_owned()));
                }
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid("content-length is not a decimal length".to_owned()));
                }
                length = Some(
                    value
                        .parse()
                        .map_err(|e| invalid(format!("content-length: {e}")))?,
                );
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                return Err(invalid("transfer-encoding is not accepted".to_owned()));
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
        }
    }
    Ok(Head {
        method,
        path,
        authorization,
        length: length.unwrap_or(0),
    })
}

/// #1710: read the body `head` declares (at most [`MAX_BODY_BYTES`],
/// checked by the caller). The body earns its time as it arrives: by the
/// time `n` bytes came, it has had [`TRANSFER_GRACE`] plus `n` at
/// [`MIN_READ_BYTES_PER_SEC`], so a peer declaring a large body and
/// trickling it is cut off near the grace, not after the time its declared
/// length would earn. The buffer grows in steps, doubling at most and never
/// past the declared length, so a declared length alone allocates nothing.
pub(super) fn read_body(
    reader: &mut BufReader<Deadlined<'_>>,
    head: Head,
) -> Result<Request, (u16, EngineRefusal)> {
    read_body_within(reader, head, TRANSFER_GRACE)
}

fn read_body_within(
    reader: &mut BufReader<Deadlined<'_>>,
    head: Head,
    grace: Duration,
) -> Result<Request, (u16, EngineRefusal)> {
    let short = |detail: String| {
        (
            400,
            refusal(
                "analytics.invalid",
                &format!("the body is shorter than content-length: {detail}"),
            ),
        )
    };
    let started = Instant::now();
    let mut body = Vec::new();
    while body.len() < head.length {
        let earned = budget(grace, body.len(), MIN_READ_BYTES_PER_SEC);
        reader.get_mut().deadline = started
            .checked_add(earned)
            .ok_or_else(|| short("the host clock cannot bound this body".to_owned()))?;
        if body.len() == body.capacity() {
            let grown = body.len().saturating_mul(2).max(BODY_STEP).min(head.length);
            body.reserve_exact(grown.saturating_sub(body.len()));
        }
        // Never more than the spare capacity: the read cannot grow the buffer.
        let step = body
            .capacity()
            .saturating_sub(body.len())
            .min(head.length.saturating_sub(body.len()))
            .min(BODY_STEP);
        let read = reader
            .by_ref()
            .take(u64::try_from(step).unwrap_or(u64::MAX))
            .read_to_end(&mut body)
            .map_err(|e| short(e.to_string()))?;
        if read == 0 {
            break;
        }
    }
    if body.len() != head.length {
        return Err(short(format!("{} of {} bytes", body.len(), head.length)));
    }
    Ok(Request {
        method: head.method,
        path: head.path,
        body,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;

    /// A connected (client, server) pair on loopback.
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    /// Send `client` one byte every 20 ms, each well inside the per-read
    /// wait, until told to stop (or the peer is gone).
    fn trickle(mut client: TcpStream) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (stop, stopped) = mpsc::channel();
        let trickling = std::thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(Duration::from_millis(20))
            {
                if client.write_all(b"x").is_err() {
                    break;
                }
            }
        });
        (stop, trickling)
    }

    fn detail(refused: Result<impl Sized, (u16, EngineRefusal)>) -> String {
        match refused {
            Ok(_) => panic!("the transport accepted what it must refuse"),
            Err((status, refused)) => format!("{status} {}", refused.detail),
        }
    }

    /// A peer sending one byte at a time, each well inside the per-read
    /// wait, is cut off at the absolute deadline, not served forever.
    #[test]
    fn a_trickling_head_is_cut_off_at_its_deadline() {
        let (client, server) = pair();
        let (stop, trickling) = trickle(client);
        let started = Instant::now();
        let mut reader =
            BufReader::new(Deadlined::new(&server, Duration::from_millis(300)).unwrap());
        let refused = detail(read_head(&mut reader));
        assert!(refused.starts_with("400 unreadable request"), "{refused}");
        assert!(started.elapsed() < Duration::from_secs(2), "{refused}");
        drop(server);
        let _stopped = stop.send(());
        trickling.join().unwrap();
    }

    /// A head line longer than the head budget is refused once the budget
    /// is spent, without waiting for the line to end.
    #[test]
    fn an_over_long_head_is_refused_as_it_arrives() {
        let (mut client, server) = pair();
        let sender = std::thread::spawn(move || {
            let _sent = client.write_all(&vec![b'a'; MAX_HEAD_BYTES.saturating_mul(4)]);
            client
        });
        let mut reader = BufReader::new(Deadlined::new(&server, Duration::from_secs(5)).unwrap());
        let refused = detail(read_head(&mut reader));
        assert_eq!(refused, "400 the request head is missing or too long");
        drop(sender.join().unwrap());
    }

    /// The head is read, the body is read to its declared length, and a
    /// body that ends short is refused.
    #[test]
    fn a_body_is_read_to_its_declared_length() {
        let (mut client, server) = pair();
        client
            .write_all(
                b"POST /v1/frontier HTTP/1.1\r\nAuthorization: Bearer k\r\ncontent-length: 5\r\n\r\nhello",
            )
            .unwrap();
        let mut reader = BufReader::new(Deadlined::new(&server, Duration::from_secs(5)).unwrap());
        let head = read_head(&mut reader).unwrap();
        assert_eq!(
            (head.method.as_str(), head.path.as_str(), head.length),
            ("POST", "/v1/frontier", 5)
        );
        assert_eq!(head.authorization.as_deref(), Some("Bearer k"));
        let request = read_body(&mut reader, head).unwrap();
        assert_eq!(request.body, b"hello");

        let (mut client, server) = pair();
        client
            .write_all(b"POST /v1/sync HTTP/1.1\r\ncontent-length: 9\r\n\r\nshort")
            .unwrap();
        drop(client);
        let mut reader = BufReader::new(Deadlined::new(&server, Duration::from_secs(5)).unwrap());
        let head = read_head(&mut reader).unwrap();
        let refused = detail(read_body(&mut reader, head));
        assert!(refused.contains("5 of 9 bytes"), "{refused}");
    }

    /// A write to a peer that never reads ends at the deadline.
    #[test]
    fn a_write_to_a_peer_that_never_reads_ends_at_its_deadline() {
        let (_client, server) = pair();
        let started = Instant::now();
        let mut out = Deadlined::new(&server, Duration::from_millis(300)).unwrap();
        let chunk = vec![0_u8; 1_048_576];
        let mut failed = false;
        for _ in 0..256 {
            if out.write_all(&chunk).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_transfer_is_granted_its_grace_and_its_bytes_at_the_floor_rate() {
        assert_eq!(write_budget(0_usize), TRANSFER_GRACE);
        // An answer: 20 MiB at 128 KiB/s is 160 s, plus the grace.
        assert_eq!(
            write_budget(20_971_520_usize),
            TRANSFER_GRACE.saturating_add(Duration::from_secs(160))
        );
        assert_eq!(
            write_budget(u64::MAX),
            TRANSFER_GRACE.saturating_add(Duration::from_millis(140_737_488_355_327_999))
        );
        // A body: what arrived, at 1 MiB/s.
        assert_eq!(
            budget(TRANSFER_GRACE, MAX_BODY_BYTES, MIN_READ_BYTES_PER_SEC),
            TRANSFER_GRACE.saturating_add(Duration::from_secs(64))
        );
        assert_eq!(
            budget(TRANSFER_GRACE, 524_288_usize, MIN_READ_BYTES_PER_SEC),
            TRANSFER_GRACE.saturating_add(Duration::from_millis(500))
        );
    }

    /// A peer declaring the largest body and trickling it is cut off near
    /// the grace, not after the minute its declared length would earn.
    #[test]
    fn a_trickled_large_body_is_cut_off_near_its_grace() {
        let (mut client, server) = pair();
        let declared =
            format!("POST /v1/sync HTTP/1.1\r\ncontent-length: {MAX_BODY_BYTES}\r\n\r\n");
        client.write_all(declared.as_bytes()).unwrap();
        let (stop, trickling) = trickle(client);
        let mut reader = BufReader::new(Deadlined::new(&server, Duration::from_secs(5)).unwrap());
        let head = read_head(&mut reader).unwrap();
        let started = Instant::now();
        let refused = detail(read_body_within(
            &mut reader,
            head,
            Duration::from_millis(300),
        ));
        assert!(refused.starts_with("400 the body is shorter"), "{refused}");
        assert!(started.elapsed() < Duration::from_secs(2), "{refused}");
        drop(server);
        let _stopped = stop.send(());
        trickling.join().unwrap();
    }

    /// A body sent at speed is read whole, and its buffer is never more
    /// than its declared length.
    #[test]
    fn a_fast_body_is_read_whole_into_a_buffer_of_its_length() {
        let (mut client, server) = pair();
        let length = 3_145_745_usize;
        let sender = std::thread::spawn(move || {
            let head = format!("POST /v1/sync HTTP/1.1\r\ncontent-length: {length}\r\n\r\n");
            client.write_all(head.as_bytes()).unwrap();
            client.write_all(&vec![7_u8; length]).unwrap();
            client
        });
        let mut reader = BufReader::new(Deadlined::new(&server, Duration::from_secs(5)).unwrap());
        let head = read_head(&mut reader).unwrap();
        let request = read_body_within(&mut reader, head, Duration::from_secs(5)).unwrap();
        assert_eq!(request.body.len(), length);
        assert!(request.body.iter().all(|byte| *byte == 7));
        assert_eq!(request.body.capacity(), length);
        drop(sender.join().unwrap());
    }

    /// A head whose body boundary is ambiguous is refused: a repeated or
    /// signed length, a length that is not decimal, or a transfer coding.
    #[test]
    fn an_ambiguous_body_boundary_is_refused() {
        for (headers, why) in [
            (
                "content-length: 3\r\ncontent-length: 3\r\n",
                "content-length is repeated",
            ),
            (
                "content-length: +3\r\n",
                "content-length is not a decimal length",
            ),
            (
                "content-length: \r\n",
                "content-length is not a decimal length",
            ),
            (
                "content-length: 0x3\r\n",
                "content-length is not a decimal length",
            ),
            (
                "transfer-encoding: chunked\r\n",
                "transfer-encoding is not accepted",
            ),
        ] {
            let (mut client, server) = pair();
            let head = format!("POST /v1/sync HTTP/1.1\r\n{headers}\r\nabc");
            client.write_all(head.as_bytes()).unwrap();
            let mut reader =
                BufReader::new(Deadlined::new(&server, Duration::from_secs(5)).unwrap());
            assert_eq!(
                detail(read_head(&mut reader)),
                format!("400 {why}"),
                "{headers}"
            );
        }
    }
}
