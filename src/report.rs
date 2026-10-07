//! Report what this chip is doing to an HTTP API, every five minutes, over TCP.
//!
//! This is the first thing in the firmware that talks to something outside the local network. The
//! other two — the radio and the time server — are a name to resolve and a socket on the LAN; this
//! one is a host on the internet, a connection that can be refused or reset in the middle of an
//! exchange, and a reply that is untrusted input. So everything the radio can do wrong has a state
//! published for the greeting in [`crate::wifi`], and everything here has a sentence and a line in
//! the log, decided in `poc-report` and printed here.
//!
//! ## What it sends, and what it expects back
//!
//! One event every [`REPORT_EVERY_SECS`], carrying the state line as its payload. The body, the
//! timestamp's format, the request head and the reading of a status line are all in `poc-report`
//! because those are decisions a host can check; this file is the part that needs a network.
//!
//! **It sends no credentials.** There is no `Authorization` header, so the API answers 401 and
//! [`poc_report::Verdict::Unauthorized`] is what this logs. That is the point of the exercise as it
//! stands: a 401 says the request reached the API, was understood, and was refused for want of a
//! token — three facts in one status line, where a timeout says only the first. Adding the token is
//! the next piece of work, and nothing here has to change for it.
//!
//! ## What is not here yet
//!
//! - **TLS.** The API is behind `https://` and this speaks plain HTTP to port 80, which is also why
//!   the token must not be added before a TLS stack is: a bearer token over cleartext is a password
//!   on the wire. The `tcp` feature of `embassy-net` in `Cargo.toml` is the whole of the transport
//!   this file uses.
//! - **A retry that is not "wait five minutes".** One attempt per interval, and a failure to reach
//!   the API is a line in the log. It is not a state on the greeting, because the greeting is about
//!   this chip and the API is somebody else's server.

use core::fmt::Write as _;
use core::net::Ipv4Addr;

use defmt::{error, info, warn};
use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use poc_report::{
    EVENT_TELEMETRY, EVENTS_PATH, Event, REPORT_EVERY_SECS, Request, Time, Verdict,
    status_line_arrived,
};

use crate::{clock, status};

/// Which API to report to, from the build's configuration.
///
/// **Read from `.cargo/esp-config.toml`,** in its `[env]` section, which is where this repository
/// keeps build-time values. `EVENTS_API_HOST` and `EVENTS_API_PORT` name the two halves, and
/// `.cargo/local.toml` overrides either of them for a machine that needs it — a `wrangler dev` on the
/// LAN, another account, a staging worker.
///
/// The two constants below are the fallback rather than the answer. They name the same deployed Worker
/// the configuration file does, which is deliberate twice over: the gate and CI build with no
/// `local.toml` at all, and `esp-generate` owns `.cargo/esp-config.toml`, so a regeneration can drop
/// the keys. With the constants here, either case still builds and still points at the same API.
///
/// The host is printed in the log lines about a name not resolving and nowhere else: it is fixed at
/// build time, so a reader of any other line already knows which API this build talks to, and a name
/// repeated in every line of a log is a name nobody reads.
const HOST: &str = match option_env!("EVENTS_API_HOST") {
    Some(host) => host,
    None => "cfpoc.andresmoschini.workers.dev",
};

/// The port to connect to when the build says nothing: 80.
///
/// 80 rather than 443 because there is no TLS in this firmware, and a plain-HTTP request to a
/// Cloudflare worker arrives on 80 and is answered there — measured on 2026-10-07 against the
/// deployed worker, not assumed.
const DEFAULT_PORT: u16 = 80;

/// The port from the build's configuration, or [`DEFAULT_PORT`].
///
/// `EVENTS_API_PORT` is a separate value from the host rather than part of it, because `wrangler dev`
/// does not listen on 80 and moves to 8788 when 8787 is taken: a combined `host:port` would be a URL
/// that is wrong in two different ways at once.
///
/// A function rather than a `const` because parsing a port out of a string at compile time means
/// writing a parser, and `core`'s does the whole job — including refusing `"8787 "` and `"0x8787"` —
/// for one call every five minutes.
fn port() -> u16 {
    match option_env!("EVENTS_API_PORT") {
        Some(port) => port.trim().parse().unwrap_or(DEFAULT_PORT),
        None => DEFAULT_PORT,
    }
}

/// The chip, as [`device_id`] names it.
///
/// A `cfg` pair rather than a constant because the two images of this firmware are two different
/// chips and nothing reaches this file from `Cargo.toml`: the feature selects what is built, and this
/// is one of the two places the firmware has to be told which chip it is on. The other is the
/// reserved-pin list in `src/bin/main.rs`.
#[cfg(feature = "esp32c3")]
const CHIP: &str = "esp32c3";

/// The chip, for the other image of this firmware.
#[cfg(feature = "esp32c6")]
const CHIP: &str = "esp32c6";

/// How long any single step of an exchange may take.
///
/// Each step is timed on its own, as in `src/ntp.rs`, so that a line saying an exchange failed also
/// says which part of it failed: a name that does not resolve and a server that does not answer are
/// different problems with different fixes.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Receive buffer for the socket.
///
/// Larger than any reply this API gives — the largest is a Cloudflare error page of a few hundred
/// bytes. `smoltcp` drops a datagram that does not fit rather than truncating it, so a buffer sized
/// to the status line alone would throw away every body there is.
const RX_LEN: usize = 1024;

/// Transmit buffer for the socket, which has to hold the head and the body.
const TX_LEN: usize = 512;

/// How many bytes of body to build.
///
/// 512, comfortably over the longest body this firmware can produce: a failed join with a signal in it
/// and a clock counting from boot, which `poc-report`'s tests measure at under 300. The buffer is
/// sized over the worst case rather than the usual one, because a body that does not fit is a report
/// that is silently not sent.
const BODY_LEN: usize = 512;

/// How many bytes of head to build.
///
/// A fixed set of headers, a path and a number, over [`Request`]'s worst case by a wide margin: the
/// only variable part is the host name, and a DNS name is at most 253 characters.
const HEAD_LEN: usize = 256;

/// How many bytes [`device_id`] writes.
///
/// A MAC address is six bytes, written as `esp32c3-aabbccddeeff`: the chip, so two firmware images on
/// two chips do not share an id space, and the address without separators, because this value is
/// matched on rather than read.
const ID_LEN: usize = 32;

/// Starts reporting, and returns once the task is spawned.
///
/// Nothing is returned and nothing is waited for: the whole reporter is one task, spawned from `main`
/// once the network stack exists, and every step inside it is bounded by a timeout of its own. A
/// network that never comes up costs this task nothing but its own waiting.
///
/// # Panics
///
/// If the executor has no room left for another task. All four are allocated once, at boot, so this
/// is a fact about the size of the task pool rather than something that can happen later.
pub fn start(spawner: Spawner, stack: Stack<'static>) {
    // Opened here rather than inside the task below, for the reason `src/ntp.rs` opens its socket in
    // the same place: `make_static!` builds its type out of `impl Trait`, and a task's body is itself
    // an opaque type to the compiler, so one inside the other is a cycle it cannot resolve.
    let socket = open(stack);

    spawner.spawn(report(stack, socket).expect("report is a task"));
}

/// Opens the one socket this reporter uses.
///
/// The buffers are statics behind `make_static!` because the socket borrows them for as long as it
/// lives, and it is opened once for the whole life of the firmware: `embassy-net` has a fixed number
/// of sockets and a socket set that is full panics rather than refusing, so a socket opened per
/// attempt would be a reporter that works once and then stops.
fn open(stack: Stack<'static>) -> TcpSocket<'static> {
    TcpSocket::new(
        stack,
        static_cell::make_static!([0; RX_LEN]),
        static_cell::make_static!([0; TX_LEN]),
    )
}

/// The task behind [`start`].
#[embassy_executor::task]
async fn report(stack: Stack<'static>, mut socket: TcpSocket<'static>) {
    // The first report waits for DHCP rather than firing into a stack that has no address yet: the
    // exchange needs an address to send from, and the resolver it needs to find the API with is
    // configured by the same lease.
    stack.wait_config_up().await;

    loop {
        once(&stack, &mut socket).await;

        Timer::after(Duration::from_secs(REPORT_EVERY_SECS)).await;
    }
}

/// One exchange: resolve the host, connect, write the head and the body, and read the answer.
async fn once(stack: &Stack<'static>, socket: &mut TcpSocket<'static>) {
    let Ok(address) = resolve(stack).await else {
        return;
    };

    // Read here, in the task that is about to report, rather than passed in: the greeting reads the
    // same three facts twice a second and this reads them once every five minutes, and the state a
    // report carries has to be the state at the moment it is stamped.
    let state = status::report(Some(*stack));

    let mut id = [0; ID_LEN];
    let id_len = write_id(&mut id, esp_hal::efuse::base_mac_address());

    // The bytes are ASCII because a hex digit is ASCII and nothing else was written into them, so
    // this cannot fail. An `expect` on something that cannot is the honest way to say so without
    // writing `unsafe`.
    let device_id = core::str::from_utf8(&id[..id_len]).expect("a hex device id is ASCII");

    let event = Event {
        device_id,
        timestamp_secs: now_secs(),
        event_type: EVENT_TELEMETRY,
        status: &state,
    };

    // The body is built before the head because `Content-Length` is its length and the head goes
    // first on the wire. A body that does not fit is a report that is not sent, and [`fill`] says so
    // in the log rather than sending a truncated one.
    let mut body = [0; BODY_LEN];
    let Some(body_len) = fill(&mut body, &event) else {
        return;
    };

    let mut head = [0; HEAD_LEN];
    let Some(head_len) = fill(
        &mut head,
        &Request {
            host: HOST,
            path: EVENTS_PATH,
            content_length: body_len,
        },
    ) else {
        return;
    };

    // Each step below returns `Err` having already said what went wrong, so the caller only has to
    // stop. The socket is dropped rather than reused on a failure: a `connect` that timed out leaves
    // `smoltcp`'s state machine mid-handshake, and the next attempt on this socket would be refused
    // as an invalid state rather than opening a new connection. Dropping it takes it out of the
    // socket set, and the next pass opens a fresh one.
    if connect(socket, address).await.is_err() {
        return;
    }

    if send(socket, &head[..head_len]).await.is_err() {
        return;
    }

    if send(socket, &body[..body_len]).await.is_err() {
        return;
    }

    // Half-closed rather than left open: the head says `Connection: close`, so the server is about to
    // shut its half, and reading before its FIN arrives would wait for bytes that are not coming.
    // `close` shuts only the write half, which is exactly "I have finished asking".
    socket.close();

    let Some(verdict) = read_reply(socket).await else {
        return;
    };

    // `info` for the one answer that means the exchange worked, `warn` for the rest, and `error` for
    // neither. A 401 is the expected answer while there is no token, and a line marked as an error
    // every five minutes is a line nobody reads — the words in it are still what to do about it.
    if verdict == Verdict::Stored {
        info!(
            "reported to the API after {}",
            defmt::Display2Format(&poc_report::age(Instant::now().as_secs()))
        );
    } else {
        warn!(
            "the API did not store the event: {}",
            defmt::Display2Format(&verdict)
        );
    }
}

/// Reads until the reply's first line is whole, and says what it said.
///
/// The loop is the whole point of this function. A TCP read returns whatever has arrived, which is
/// not the same thing as a reply: the 401 this API sends is 650 bytes against a 25-byte status line,
/// and nothing in TCP promises where the boundary between them falls. Reading once and judging that
/// is what this firmware used to do, and it reported "what answered was not the API" for replies the
/// API had sent correctly — because the read had landed inside the status line and a line with eleven
/// of its twenty-five bytes looks like no line at all.
///
/// Three ways to stop, and each is a different thing to say:
///
/// - the status line is whole, which is the answer;
/// - the buffer is full, which is a reply longer than [`RX_LEN`] and a sentence about that;
/// - the peer closed with nothing more, which is a server that answered and then stopped.
///
/// `None` means there was no reply to judge, and each of those has already said so.
async fn read_reply(socket: &mut TcpSocket<'static>) -> Option<Verdict> {
    let mut buffer = [0; RX_LEN];
    let mut read = 0;

    while read < buffer.len() {
        // Ask before reading again, on the bytes from the previous read: this is the answer to "is
        // there anything left to wait for", and it can be true before the read that completes it.
        if status_line_arrived(&buffer[..read]) {
            return Some(Verdict::from_reply(&buffer[..read]));
        }

        let got = match with_timeout(TIMEOUT, socket.read(&mut buffer[read..])).await {
            Err(_) => {
                error!("the API did not finish answering after {} bytes", read);

                return None;
            }
            Ok(Err(e)) => {
                error!("the answer could not be read: {:?}", e);

                return None;
            }
            Ok(Ok(got)) => got,
        };

        // Zero bytes with the connection open is the end of the stream: the server said everything it
        // was going to say. A status line inside it is still a status line, so this is checked
        // before giving up rather than after.
        if got == 0 {
            return Some(Verdict::from_reply(&buffer[..read]));
        }

        read += got;
    }

    // The buffer is full and no CRLF is in it, so the first line is longer than the whole buffer.
    // That is not a reply this firmware can read, and saying so beats reporting a sentence about a
    // reply it has not finished collecting.
    if status_line_arrived(&buffer) {
        return Some(Verdict::from_reply(&buffer));
    }

    error!(
        "the first line of the reply is longer than the {} byte buffer",
        RX_LEN
    );

    None
}

/// Opens the connection to the API, or says why it did not.
///
/// Its own function rather than a step inside [`once`] because it is the one step whose failure is
/// not about the request: everything after it is about bytes this firmware is sending, and a reader
/// of a log line saying which step failed needs the two to be separable.
async fn connect(socket: &mut TcpSocket<'static>, address: Ipv4Addr) -> Result<(), ()> {
    match with_timeout(TIMEOUT, socket.connect((address, port()))).await {
        Err(_) => {
            error!("the API did not complete the connection: it timed out");

            Err(())
        }
        // The driver's own words are logged here rather than folded into a sentence: what a
        // connection was refused for is a numbered reason out of a set this firmware does not name,
        // and a reader of this log is better served by it than by a guess.
        Ok(Err(e)) => {
            error!("the API would not accept the connection: {:?}", e);

            Err(())
        }
        Ok(Ok(())) => Ok(()),
    }
}

/// Writes all of `bytes` to the socket, or gives up.
///
/// `write` says how many bytes it took, and on a socket with a 512-byte transmit buffer and a request
/// longer than that it is routinely fewer than all of them. The loop is not optional: a request sent
/// in two pieces with the second missing is a request the server waits on.
///
/// `Err(())` rather than the driver's error because every failure here has already said what it was
/// in the log, and a caller that logged it again would print the same sentence twice.
async fn send(socket: &mut TcpSocket<'static>, bytes: &[u8]) -> Result<(), ()> {
    let mut sent = 0;

    while sent < bytes.len() {
        let wrote = match with_timeout(TIMEOUT, socket.write(&bytes[sent..])).await {
            Err(_) => {
                error!(
                    "the request did not go out: it timed out after {} bytes",
                    sent
                );

                return Err(());
            }
            Ok(Err(e)) => {
                error!(
                    "the request could not be written after {} bytes: {:?}",
                    sent, e
                );

                return Err(());
            }
            Ok(Ok(wrote)) => wrote,
        };

        // A write of nothing while the socket claims it can send is the case that would otherwise
        // spin here forever: the connection is open, there is no room and there is no progress. It is
        // what a server that accepted the connection and then stopped reading looks like.
        if wrote == 0 {
            error!(
                "the request stopped going out after {} of {} bytes",
                sent,
                bytes.len()
            );

            return Err(());
        }

        sent += wrote;
    }

    match socket.flush().await {
        Ok(()) => Ok(()),
        Err(e) => {
            error!("the request could not be flushed: {:?}", e);

            Err(())
        }
    }
}

/// The address of [`HOST`], once DHCP has given the resolver some to ask.
///
/// Its own function rather than the one in `src/ntp.rs` because that one is private to it and this is
/// another server in another exchange. The two refusals mean different things, so the sentences are
/// different too: a name that does not resolve is DNS or the network, and the name is in both
/// sentences because a log line about a name is not answerable without the name.
async fn resolve(stack: &Stack<'static>) -> Result<Ipv4Addr, ()> {
    let found = match with_timeout(TIMEOUT, stack.dns_query(HOST, DnsQueryType::A)).await {
        Err(_) => {
            error!("the name of the API did not resolve in time: {}", HOST);

            return Err(());
        }
        Ok(Err(e)) => {
            error!("the name of the API did not resolve: {} ({:?})", HOST, e);

            return Err(());
        }
        Ok(Ok(found)) => found,
    };

    // An A record is a question about IPv4 and this firmware has no other protocol to send over, so an
    // answer that is empty or IPv6-only is not a name that did not exist: it is a name this cannot be
    // reached at.
    if let Some(IpAddress::Ipv4(address)) = found.first() {
        return Ok(*address);
    }

    error!("the name of the API is not one this can send to: {}", HOST);

    Err(())
}

/// Writes this chip's name into `into`: the chip, a dash, and the MAC address in hex.
///
/// Six bytes in and twelve out, so [`ID_LEN`] is twice what is needed and there is room for the chip
/// name and the dash. A length rather than a `&str` because the buffer belongs to the caller, which is
/// what lets this run on the stack of the task that is reporting.
fn write_id(into: &mut [u8; ID_LEN], mac: esp_hal::efuse::MacAddress) -> usize {
    let mut writer = Slice {
        buffer: into,
        written: 0,
    };

    // The `.ok()`s rather than an `unwrap`: a `fmt::Write` into a fixed buffer fails when it is full,
    // and this one is sized from what goes into it — twelve hex digits and at most seven for the chip
    // name, in a buffer of thirty-two. If that ever stops being true the id is written as far as it
    // went, which is visible in the API's table, rather than a panic in a network task.
    write!(writer, "{CHIP}-").ok();

    for byte in mac.as_bytes() {
        write!(writer, "{byte:02x}").ok();
    }

    writer.written
}

/// The time the event is stamped with, in seconds since the epoch.
///
/// The clock's reading rather than the scheduler's, because an event stamped with a count from boot
/// says 1970 and one stamped with a real date can be queried. Before a server has answered there is
/// no such thing as the date on this chip, so this is `0` — 1970-01-01T00:00:00Z, which is what a chip
/// that has not reached a time server believes, and which the state line in the payload says out loud.
fn now_secs() -> u64 {
    match clock::time() {
        Time::FromServer { epoch_secs, .. } => epoch_secs,
        Time::SinceBoot { .. } => 0,
    }
}

/// Formats one value into a buffer, and says how many bytes it took.
///
/// `None` when the value does not fit, which is a report that is not sent. A function rather than an
/// inline `write!` because that failure is worth one line in the log naming the buffer, and
/// `core::fmt::Error` says nothing of the sort.
fn fill<const N: usize>(buffer: &mut [u8; N], value: &impl core::fmt::Display) -> Option<usize> {
    let mut writer = Slice { buffer, written: 0 };

    match write!(writer, "{value}") {
        Ok(()) => Some(writer.written),
        Err(e) => {
            error!("a {} byte buffer was too small for this report: {:?}", N, e);

            None
        }
    }
}

/// A [`core::fmt::Write`] that writes into a fixed slice and counts what it wrote.
///
/// `core` has no such thing and `heapless` would be a dependency for one adapter. `Err` on a full
/// buffer is what [`core::fmt::Write`] says to do, and the count is what `Content-Length` needs.
struct Slice<'a, const N: usize> {
    /// Where the bytes go.
    buffer: &'a mut [u8; N],

    /// How many of them have gone so far.
    written: usize,
}

impl<const N: usize> core::fmt::Write for Slice<'_, N> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let room = self
            .buffer
            .get_mut(self.written..self.written + text.len())
            .ok_or(core::fmt::Error)?;

        room.copy_from_slice(text.as_bytes());

        self.written += text.len();

        Ok(())
    }
}
