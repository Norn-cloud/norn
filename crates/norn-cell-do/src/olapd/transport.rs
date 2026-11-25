//! #1710: `norn-olapd`'s bounded HTTP/1.1 transport. No peer holds a
//! worker for long: the head must arrive within [`HEAD_DEADLINE`] of the
//! accept and within [`MAX_HEAD_BYTES`], read no further than that budget;
//! a body, like every answer, has an absolute deadline sized to its length
//! ([`transfer_budget`]), not a per-read timeout a trickling peer could renew
//! forever; and a body's buffer grows only as its bytes arrive.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
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
/// #1710: the time any transfer is granted besides its length: a body
/// arrives, or an answer is sent, within this plus its length at
/// [`MIN_BYTES_PER_SEC`].
const TRANSFER_GRACE: Duration = Duration::from_secs(10);
/// #1710: the slowest transfer rate a peer may hold a worker at.
const MIN_BYTES_PER_SEC: u64 = 1024 * 1024;
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

/// A request as received: its head and its whole body.
pub(super) struct Request {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) authorization: Option<String>,
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

/// What a transfer of `bytes` is granted: [`TRANSFER_GRACE`] plus its
/// length at [`MIN_BYTES_PER_SEC`].
pub(super) fn transfer_budget(bytes: impl TryInto<u64>) -> Duration {
    let bytes = bytes.try_into().unwrap_or(u64::MAX);
    TRANSFER_GRACE.saturating_add(Duration::from_secs(
        bytes.checked_div(MIN_BYTES_PER_SEC).unwrap_or(u64::MAX),
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
    let (mut length, mut authorization) = (0_usize, None);
    loop {
        next(reader, &mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value
                    .trim()
                    .parse()
                    .map_err(|e| invalid(format!("content-length: {e}")))?;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
        }
    }
    Ok(Head {
        method,
        path,
        authorization,
        length,
    })
}

/// #1710: read the body `head` declares (at most [`MAX_BODY_BYTES`],
/// checked by the caller) within its own deadline. The buffer grows only as
/// bytes arrive, so a declared length alone allocates nothing.
pub(super) fn read_body(
    reader: &mut BufReader<Deadlined<'_>>,
    head: Head,
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
    reader
        .get_mut()
        .renew(transfer_budget(head.length))
        .map_err(|e| short(e.to_string()))?;
    let mut body = Vec::with_capacity(head.length.min(MAX_HEAD_BYTES));
    reader
        .take(u64::try_from(head.length).unwrap_or(u64::MAX))
        .read_to_end(&mut body)
        .map_err(|e| short(e.to_string()))?;
    if body.len() != head.length {
        return Err(short(format!("{} of {} bytes", body.len(), head.length)));
    }
    Ok(Request {
        method: head.method,
        path: head.path,
        authorization: head.authorization,
        body,
    })
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    /// A connected (client, server) pair on loopback.
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
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
        let (mut client, server) = pair();
        let trickle = std::thread::spawn(move || {
            for _ in 0..200 {
                if client.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let mut reader =
            BufReader::new(Deadlined::new(&server, Duration::from_millis(300)).unwrap());
        let refused = detail(read_head(&mut reader));
        assert!(refused.starts_with("400 unreadable request"), "{refused}");
        assert!(started.elapsed() < Duration::from_secs(2), "{refused}");
        drop(server);
        trickle.join().unwrap();
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
    fn a_transfer_is_granted_its_grace_and_its_length_at_the_floor_rate() {
        assert_eq!(transfer_budget(0_usize), TRANSFER_GRACE);
        assert_eq!(
            transfer_budget(MAX_BODY_BYTES),
            TRANSFER_GRACE.saturating_add(Duration::from_secs(64))
        );
        assert_eq!(
            transfer_budget(u64::MAX),
            TRANSFER_GRACE.saturating_add(Duration::from_secs(
                u64::MAX.checked_div(MIN_BYTES_PER_SEC).unwrap()
            ))
        );
    }
}
