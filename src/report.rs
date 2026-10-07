//! Report what this chip is doing to an HTTPS API, every five minutes, over TCP.
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
//! ## What is here now, and what is not
//!
//! - **TLS, and therefore a bearer token would be safe to add.** The exchange runs through
//!   [`crate::tls`], so the port is 443 and the line [`start`] prints says `https`. What that buys
//!   is the whole reason the token can come next: a credential in cleartext is a password on the
//!   wire. What it does not buy is a check that the certificate has not expired — see the note in
//!   [`crate::tls`], which is a property of how `MbedTLS` was built rather than of this code.
//! - **A retry that is not "wait five minutes".** One attempt per interval, and a failure to reach
//!   the API is a line in the log. It is not a state on the greeting, because the greeting is about
//!   this chip and the API is somebody else's server.

use core::ffi::CStr;
use core::fmt::Write as _;
use core::net::Ipv4Addr;

use defmt::{error, info, warn};
use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::rng::Trng;
use mbedtls_rs::{Tls, TlsReference};
use poc_report::{
    EVENT_TELEMETRY, EVENTS_PATH, Event, REPORT_EVERY_SECS, Reply, Request, Time, Verdict,
    status_line_arrived,
};

use crate::{clock, status, tls};

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
/// The host is printed once, in the line [`start`] logs before the task exists, and again in the
/// lines about a name that does not resolve — those are the two moments where knowing it is the
/// difference between a diagnosis and a guess. Everywhere else it would be noise: it is fixed at
/// build time, and a name repeated in every line of a log is a name nobody reads.
const HOST: &str = match option_env!("EVENTS_API_HOST") {
    Some(host) => host,
    None => "cfpoc.andresmoschini.workers.dev",
};

/// The port to connect to when the build says nothing: 443.
///
/// 443 rather than 80 because this speaks TLS, which is the whole point of [`crate::tls`]. It is the
/// only value here that follows from a decision rather than from a measurement, so it is written as
/// one: `EVENTS_API_PORT` is still there for the cases where 443 is the wrong answer.
const DEFAULT_PORT: u16 = 443;

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

/// How many bytes [`server_name`] writes, including the NUL it ends with.
///
/// 254 rather than a tighter bound because a DNS name is at most 253 characters and the NUL is the
/// 254th byte. [`HOST`] is a build-time value, so this buffer holds it once for the life of the
/// program rather than per exchange.
const NAME_LEN: usize = 254;

/// [`HOST`] as `MbedTLS` wants it: NUL-terminated, and living as long as the program.
///
/// `MbedTLS` takes the server name as a C string, and the TLS session borrows it for as long as the
/// session lives, so it cannot be a local of an exchange. A static is the honest way to say so: the
/// name is fixed at build time, so this is a value that never changes, and building it once keeps
/// the borrow out of the reporter's own types.
///
/// A `CStr` cannot be a `const` here because [`HOST`] may come from the environment through
/// `option_env!`, and `concat!` — the only way to append a NUL at compile time — needs literals.
/// The copy is [`NAME_LEN`] bytes and happens once.
fn server_name(buffer: &'static mut [u8; NAME_LEN]) -> &'static CStr {
    let len = buffer.len();

    if HOST.len() + 1 > len {
        error!(
            "the name of the API is {} bytes, which is longer than the {} byte buffer",
            HOST.len(),
            len
        );

        // Nothing was written, so the buffer is all NULs and this is an empty name rather than a
        // truncated one: a truncated host is a different name, and would fail a certificate check
        // for a reason that has nothing to do with the certificate.
        return CStr::from_bytes_until_nul(buffer).expect("a buffer of zeroes is a C string");
    }

    buffer[..HOST.len()].copy_from_slice(HOST.as_bytes());
    buffer[HOST.len()] = 0;

    CStr::from_bytes_until_nul(buffer).expect("a NUL was just written")
}

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
///
/// It is also large enough for the largest record TLS will deliver in one piece. A TLS record is at
/// most 16 KiB by `MbedTLS`' default, and `mbedtls-rs` hands the socket whatever `mbedtls_ssl_read`
/// returns, which is one record's worth of plaintext — so a peer whose first record is larger than
/// this would have it split across reads rather than truncated. Measured on the deployed Worker:
/// one 634-byte reply, arriving in one read.
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
/// `trng` is the chip's hardware random number generator, which `MbedTLS` needs for its key exchange
/// and which `esp_hal` only hands out once the entropy source has been enabled — see the comment in
/// `src/bin/main.rs`. It is taken by value because [`crate::tls::instance`] keeps it for the life of
/// the program.
///
/// # Panics
///
/// If the executor has no room left for another task. All four are allocated once, at boot, so this
/// is a fact about the size of the task pool rather than something that can happen later.
pub fn start(spawner: Spawner, stack: Stack<'static>, trng: Trng) {
    // Where this build reports to, before anything else about it, because the two halves come from
    // the environment and the reader of a serial log cannot see an environment. A log that starts
    // with "joining my-network" and never says which API it is talking to cannot answer the question
    // a reader has when the API is somebody else's: *which* API.
    //
    // Written as a URL rather than as three values because that is the form the API's own
    // documentation and its `demo.http` use, so this line can be compared against them directly. The
    // scheme is spelled out rather than left implicit because it is the one thing here that is a
    // decision rather than a value: it is `https`, and it is `https` because [`crate::tls`] is now
    // underneath it.
    info!(
        "reporting to https://{}:{}{} every {} seconds",
        HOST,
        port(),
        EVENTS_PATH,
        REPORT_EVERY_SECS,
    );

    // Here rather than inside the task, for the reason `src/ntp.rs` builds its socket in the same
    // place: `make_static!` builds its type out of `impl Trait`, and a task's body is itself an
    // opaque type to the compiler, so one inside the other is a cycle it cannot resolve. What is
    // built here is the *storage*, not the socket: the socket itself is opened per exchange by the
    // task, which is what has to own its lifetime.
    let rx = static_cell::make_static!([0; RX_LEN]);
    let tx = static_cell::make_static!([0; TX_LEN]);

    // Also here, and for the same reason: the TLS session borrows the server name for as long as it
    // lives, and `MbedTLS` has one instance for the whole program rather than one per exchange.
    let name = server_name(static_cell::make_static!([0; NAME_LEN]));
    let tls = tls::instance(trng);

    spawner.spawn(report(stack, tls, name, rx, tx).expect("report is a task"));
}

/// Opens one socket for one exchange.
///
/// The buffers are passed in rather than built here because they outlive the socket, and that
/// asymmetry is the whole of why this works. `embassy-net` has a fixed number of sockets and a
/// socket set that is full panics rather than refusing, so a socket whose buffers were also
/// per-exchange would hand the same `StaticCell` slot to `make_static!` twice and panic on the second
/// call. One pair of buffers for the life of the task, one socket at a time borrowed from them, and
/// the loop in [`report`] is what guarantees only one socket exists at a time.
fn open<'a>(stack: Stack<'a>, rx: &'a mut [u8; RX_LEN], tx: &'a mut [u8; TX_LEN]) -> TcpSocket<'a> {
    TcpSocket::new(stack, rx, tx)
}

/// The task behind [`start`].
#[embassy_executor::task]
async fn report(
    stack: Stack<'static>,
    tls: Tls<'static>,
    name: &'static CStr,
    rx: &'static mut [u8; RX_LEN],
    tx: &'static mut [u8; TX_LEN],
) {
    // The first report waits for DHCP rather than firing into a stack that has no address yet: the
    // exchange needs an address to send from, and the resolver it needs to find the API with is
    // configured by the same lease.
    stack.wait_config_up().await;

    loop {
        // A socket per exchange, dropped at the end of it. That is not a stylistic choice: a
        // `TcpSocket` that is still open cannot be connected again — `smoltcp` answers `connect`
        // on an open socket with `InvalidState`, which is what the second pass used to log five
        // minutes after the first one succeeded. Dropping it takes it out of the socket set and
        // leaves the next pass a socket that has never been connected, which is the only state
        // `connect` accepts.
        let mut socket = open(stack, rx, tx);

        once(&stack, tls.reference(), &mut socket, name).await;

        Timer::after(Duration::from_secs(REPORT_EVERY_SECS)).await;
    }
}

/// One exchange: resolve the host, connect, shake hands, write the head and the body, and read the
/// answer.
async fn once(
    stack: &Stack<'static>,
    tls: TlsReference<'_>,
    socket: &mut TcpSocket<'_>,
    name: &'static CStr,
) {
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
    // stop. Nothing here has to arrange for the socket to be reusable: the caller drops it on the way
    // out whatever the outcome, which is also what a `connect` that timed out mid-handshake needs,
    // since `smoltcp`'s state machine would refuse the next `connect` on it as an invalid state.
    if connect(socket, address).await.is_err() {
        return;
    }

    // The TCP connection is established but nothing has been said yet, so this is the first moment
    // at which a certificate could be checked and the first moment at which the API is known to be
    // the API. The borrow of the socket ends with the session, so a handshake that fails leaves the
    // socket in the state the step above left it in.
    let Some(mut stream) = tls::open(tls, socket, name).await else {
        return;
    };

    if send(&mut stream, &head[..head_len]).await.is_err() {
        return;
    }

    if send(&mut stream, &body[..body_len]).await.is_err() {
        return;
    }

    // Nothing is closed before the reply is read. The head says `Connection: close`, so the server
    // closes when it has answered, and the answer is what this is waiting for: shutting the write
    // half first measured as a `ConnectionReset` before a single byte came back, which is this
    // server's answer to being told the conversation was over before it had replied to it.
    let mut buffer = [0; RX_LEN];

    let read = read_reply(&mut stream, &mut buffer).await;

    // Finished asking, once whatever was coming has come — TLS `close_notify`, which is the
    // protocol's own way of saying there is no more of this exchange. On both paths rather than
    // only the happy one: MbedTLS warns on a session dropped while still open, and a warning that
    // fires on every failed exchange would be a warning about the reporting, not about the failure.
    match with_timeout(TIMEOUT, stream.close()).await {
        Err(_) => warn!("the exchange was not closed politely: it timed out"),
        Ok(Err(e)) => warn!("the exchange was not closed politely: {:?}", e),
        Ok(Ok(())) => {}
    }

    let Some(read) = read else {
        return;
    };

    let reply = Reply::from_bytes(&buffer[..read]);

    // The status code goes on every line, and it is the one thing here that is not this firmware's
    // opinion: it is what the API actually did, where the sentence beside it is what to make of it. A
    // reader who does not believe the sentence can still go and read the number.
    match reply.status() {
        // No code at all, for a reply that was not HTTP. Saying "no status" beats printing a zero,
        // which is not a thing this API can send and would read as a status.
        None => warn!(
            "the API did not store the event: {}",
            defmt::Display2Format(&reply.verdict())
        ),
        Some(status) if reply.verdict() == Verdict::Stored => info!(
            "the API stored the event, {} after {}",
            status,
            defmt::Display2Format(&poc_report::age(Instant::now().as_secs()))
        ),
        Some(status) => warn!(
            "the API did not store the event: {} {}",
            status,
            defmt::Display2Format(&reply.verdict())
        ),
    }

    // The API's own words, on anything that was not a store. On a 400 this is the only thing that
    // says which field was wrong; on a status this firmware does not name, it is the only thing the
    // API said at all. Left out for a store, whose body is eleven bytes saying "ok" — every five
    // minutes, forever, and a log line nobody reads is a log line that costs time to skip.
    if reply.verdict() != Verdict::Stored {
        info!(
            "the API said: {}",
            defmt::Display2Format(&poc_report::logged(reply.body()))
        );
    }
}

/// Reads until the reply's first line is whole, and says how much of it there is.
///
/// The buffer belongs to the caller, and only the count comes back: [`Reply`] borrows the bytes, and
/// a value that borrowed a local would not survive this function. That turns out to be the better
/// split anyway — this is the part that talks to the network and the caller is the part that reads
/// what arrived.
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
/// - the peer closed with nothing more, which is a server that answered and then stopped;
/// - the buffer filled up with no CRLF in it, which is a reply this firmware cannot read.
///
/// `None` means the last two, or a read that failed, and each has already said which.
async fn read_reply(stream: &mut tls::Stream<'_, '_>, into: &mut [u8; RX_LEN]) -> Option<usize> {
    let mut read = 0;

    while read < into.len() {
        // Ask before reading again, on the bytes from the previous read: this is the answer to "is
        // there anything left to wait for", and it can be true before the read that completes it.
        if status_line_arrived(&into[..read]) {
            return Some(read);
        }

        let got = match with_timeout(TIMEOUT, stream.read(&mut into[read..])).await {
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
            return Some(read);
        }

        read += got;
    }

    // The buffer is full and no CRLF is in it, so the first line is longer than the whole buffer.
    // That is not a reply this firmware can read, and saying so beats handing back a count and
    // letting the caller judge a line it has not finished collecting.
    if status_line_arrived(into) {
        return Some(read);
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
async fn connect(socket: &mut TcpSocket<'_>, address: Ipv4Addr) -> Result<(), ()> {
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

/// Writes all of `bytes` to the stream, or gives up.
///
/// `write` says how many bytes it took, and it is routinely fewer than all of them — over TLS a
/// request is cut into records of at most 16 KiB, and it may be less than that for want of room in
/// the socket's 512-byte transmit buffer. The loop is not optional: a request sent in two pieces
/// with the second missing is a request the server waits on.
///
/// `Err(())` rather than the stack's error because every failure here has already said what it was
/// in the log, and a caller that logged it again would print the same sentence twice.
async fn send(stream: &mut tls::Stream<'_, '_>, bytes: &[u8]) -> Result<(), ()> {
    let mut sent = 0;

    while sent < bytes.len() {
        let wrote = match with_timeout(TIMEOUT, stream.write(&bytes[sent..])).await {
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

    match stream.flush().await {
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
