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
//! One event every [`REPORT_EVERY_SECS`], carrying the state line as its payload. The body and the
//! timestamp's format are in `poc-report` because those are decisions a host can check; this file is
//! the part that needs a network.
//!
//! **The HTTP framing is `edge-http`'s.** The request head, the reading of a reply, and the loop that
//! keeps reading until one is whole are all [`edge_http`]: this file builds a [`RequestHeaders`],
//! writes it, hands over the body, and reads back a [`ResponseHeaders`] and a [`Body`]. That is not
//! an arbitrary line to draw. The framing is the part of an HTTP client that is fiddly in the detail
//! nobody reads — `Content-Length` versus a header the library guesses, CRLF, a reply that arrives in
//! as many reads as the network decides — and this firmware shipped the resulting bug: it read the
//! reply once and judged whatever arrived, so a read landing inside the 25-byte status line of a
//! 650-byte reply reported "what answered was not the API" for a reply the API had sent correctly.
//! What `edge-http` removes is that class of bug, and what is left here is the one thing a
//! general-purpose HTTP client cannot decide: **which API this build reports to**, and what to say
//! about what it said back.
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
//!
//! ## What `edge-http` does not decide, and this file still does
//!
//! A general-purpose HTTP client knows nothing about which API it is talking to, so everything about
//! *that* is here and is what this file is for: which host and path from the build's configuration,
//! which headers this exchange sends and why each is there, and what to say about what came back.
//! The three lines it prints are the same three lines whatever the status, because a reader of a
//! serial log should not have to know which failure they are looking at to find out what the log
//! says.

use core::ffi::CStr;
use core::fmt::Write as _;
use core::net::Ipv4Addr;

use defmt::{error, info, warn};
use edge_http::io::Body;
use edge_http::{ConnectionType, Method, RequestHeaders, ResponseHeaders};
use edge_nal::io::{Read, Write};
use edge_nal::{Close, TcpShutdown as _};
use edge_nal_embassy::{Tcp as EmbassyTcp, TcpBuffers};
use edge_nal_tls::TlsConnector;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::rng::Trng;
use poc_report::{EVENT_TELEMETRY, EVENTS_PATH, Event, REPORT_EVERY_SECS, Reply, Time, Verdict};

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

/// How many bytes of body to build.
///
/// 512, comfortably over the longest body this firmware can produce: a failed join with a signal in it
/// and a clock counting from boot, which `poc-report`'s tests measure at under 300. The buffer is
/// sized over the worst case rather than the usual one, because a body that does not fit is a report
/// that is silently not sent.
const BODY_LEN: usize = 512;

/// How many headers this exchange sends, and how many it will accept back.
///
/// `edge-http`'s `Headers` is `[httparse::Header; N]` held **by value**, so `N` is stack rather than
/// heap: its default of 64 is about a kilobyte of the reporting task's stack for no benefit, since
/// this exchange sends four headers (`Host`, `Content-Type`, `Content-Length`, `Connection`) and the
/// deployed Worker answers with ten.
///
/// Sixteen rather than four because `Headers::set` **panics** with `No space left` rather than
/// returning an error, so the count is a promise the type system does not check. Four is exactly
/// what is sent today and has no room for the `Authorization` header that is the next piece of work;
/// sixteen leaves room for it without another visit to this constant.
const HEADERS: usize = 16;

/// How many bytes of scratch `Content-Length` is rendered into.
///
/// `edge-http` takes a `heapless::String` to write the number into rather than a `&str`, because a
/// header value is text and the number is not one yet. Twenty is `Content-Length: ` plus a `u64`:
/// more than any length this body can have, and the type says so.
const LENGTH_LEN: usize = 20;

/// How many bytes of the API's answer to keep.
///
/// 256, over the largest body this API sends: the 401 is 24 bytes and the largest thing behind it is
/// a Cloudflare error page of a few hundred. The head itself goes in [`tls::RX_LEN`], which is the
/// socket's own receive buffer — `edge-http` parses the head out of that and hands back what is left,
/// which is where this one is read from.
///
/// Cut rather than refused, and said so in the log when it happens: a body too long for this is a
/// Cloudflare error page, and the first 120 characters of one already say it is HTML.
const BODY_REPLY_LEN: usize = 256;

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
    // opaque type to the compiler, so one inside the other is a cycle it cannot resolve. All of it
    // is storage rather than state: the pool of socket buffers, the server name, and the factory
    // that holds the trust anchor and the MbedTLS instance.
    let buffers = static_cell::make_static!(TcpBuffers::new());
    let name = server_name(static_cell::make_static!([0; NAME_LEN]));
    let tls = tls::instance(trng);
    let connector = tls::connector(tls, buffers, stack, name);

    spawner.spawn(report(stack, connector).expect("report is a task"));
}

/// The task behind [`start`].
#[embassy_executor::task]
async fn report(
    stack: Stack<'static>,
    connector: &'static TlsConnector<'static, EmbassyTcp<'static>>,
) {
    // The first report waits for DHCP rather than firing into a stack that has no address yet: the
    // exchange needs an address to send from, and the resolver it needs to find the API with is
    // configured by the same lease.
    stack.wait_config_up().await;

    loop {
        once(stack, connector).await;

        Timer::after(Duration::from_secs(REPORT_EVERY_SECS)).await;
    }
}

/// One exchange: resolve the host, connect, shake hands, write the head and the body, and read the
/// answer.
async fn once(
    stack: Stack<'static>,
    connector: &'static TlsConnector<'static, EmbassyTcp<'static>>,
) {
    let Ok(address) = resolve(&stack).await else {
        return;
    };

    // Read here, in the task that is about to report, rather than passed in: the greeting reads the
    // same three facts twice a second and this reads them once every five minutes, and the state a
    // report carries has to be the state at the moment it is stamped.
    let state = status::report(Some(stack));

    let id = write_id(esp_hal::efuse::base_mac_address());

    let event = Event {
        device_id: id.as_str(),
        timestamp_secs: now_secs(),
        event_type: EVENT_TELEMETRY,
        status: &state,
    };

    // The body is built before the head because `Content-Length` is its length and the head goes
    // first on the wire. A body that does not fit is a report that is not sent, and [`fill`] says so
    // in the log rather than sending a truncated one.
    let Some(body) = fill::<BODY_LEN>(&event) else {
        return;
    };

    // The scratch `Content-Length` is rendered into has to outlive the header that borrows from it:
    // `Headers::set_content_len` takes a `&'b mut` and stores a `&'b str` into it, so both live here
    // rather than the buffer inside whatever builds the head.
    let mut length = heapless::String::<LENGTH_LEN>::new();

    let mut head = RequestHeaders::<HEADERS>::new();
    head.method = Method::Post;
    head.path = EVENTS_PATH;

    // Four headers, and each is here for a reason rather than by habit. `Host` is the name this
    // firmware asked for rather than the address it resolved to, because that is what a server routes
    // on and what a log can be read against. `Content-Length` is measured rather than declared,
    // because the body was written into a buffer before any of this and a length that disagrees with
    // it is a request the server waits on until it gives up. `Connection: close` because this client
    // reads the answer and does nothing else with the connection: without it the server may hold the
    // socket open, and the read in [`exchange`] then waits for bytes that are not coming, which is a
    // timeout on every single report rather than once.
    head.headers.set_host(HOST);
    head.headers.set_content_type("application/json");
    head.headers.set_content_len(body.len() as u64, &mut length);
    head.headers.set_connection_close();

    // Each step returns having already said what went wrong, so the caller only has to stop. Nothing
    // here has to arrange for the connection to be reusable: `crate::tls` builds the socket out of a
    // pool and the connection is dropped on the way out whatever the outcome, which is also what a
    // handshake that timed out needs — `smoltcp` refuses the next `connect` on a socket that is still
    // open as an invalid state.
    //
    // This is the first moment at which a certificate could be checked, and the first at which the API
    // is known to be the API.
    let Some(mut stream) = tls::open(connector, address, port()).await else {
        return;
    };

    exchange(&mut stream, &head, body.as_bytes()).await;
}

/// One exchange over an open connection: write the head, write the body, read the answer, say what
/// it was, and close.
///
/// Its own function rather than more of [`once`] because the buffers are its locals. Between them
/// they are about a kilobyte and a half of stack — the head buffer is the socket's receive buffer —
/// and a reporting task that held them for its whole life would be holding them across every one of
/// its five-minute sleeps.
///
/// Nothing is closed before the answer is read. The head says `Connection: close`, so the server
/// closes when it has answered, and the answer is what this is waiting for: shutting the write half
/// first measured as a `ConnectionReset` before a single byte came back, which is this server's
/// answer to being told the conversation was over before it had replied to it.
async fn exchange(stream: &mut tls::Stream<'_>, head: &RequestHeaders<'_, HEADERS>, body: &[u8]) {
    // `false` for `chunked_if_unspecified`: this request has a body whose length is already known, so
    // a chunked encoding would be a second way of saying the same thing and one more thing to go
    // wrong. The head and the body then go out as two writes, which is what went on the wire before
    // `edge-http` too.
    match with_timeout(TIMEOUT, head.send(false, &mut *stream)).await {
        Err(_) => {
            error!("the request head did not go out: it timed out");

            return;
        }
        Ok(Err(e)) => {
            error!("the request head did not go out: {:?}", e);

            return;
        }
        Ok(Ok(_)) => {}
    }

    match with_timeout(TIMEOUT, stream.write_all(body)).await {
        Err(_) => {
            error!("the request body did not go out: it timed out");

            return;
        }
        Ok(Err(e)) => {
            error!("the request body did not go out: {:?}", e);

            return;
        }
        Ok(Ok(())) => {}
    }

    // The head is read out of the socket's own receive buffer, and whatever arrived in the same read
    // past the blank line is handed back as the start of the body. `false` for `exact` is the whole
    // point of this call: with `exact = true` the reader takes the head one byte at a time until it
    // finds `\r\n\r\n`, and with `false` it reads in bulk and re-parses what it has, which is what
    // makes a reply that arrives in pieces a reply rather than a series of malformed prefixes. The bug
    // this firmware shipped was reading the reply once and judging whatever arrived — against this
    // API, a 650-byte reply whose status line is 25 of those bytes, so a read landing inside the line
    // is the ordinary case rather than the exotic one. This line is where that stops being possible.
    let mut buffer = [0; tls::RX_LEN];
    let mut answer = ResponseHeaders::<HEADERS>::new();

    let head_read =
        match with_timeout(TIMEOUT, answer.receive(&mut buffer, &mut *stream, false)).await {
            Err(_) => {
                error!("the API did not finish answering in time");

                close(stream).await;

                return;
            }
            Ok(Err(edge_http::io::Error::ConnectionClosed)) => {
                // Nothing at all came back: a peer that accepted the connection and then stopped. That is
                // not the same as something answering that was not the API, and it wants a different
                // sentence — one names the API being unreachable, the other names a portal.
                close(stream).await;

                say(Reply::nothing_came_back());

                return;
            }
            Ok(Err(e)) => {
                // Something answered and it was not HTTP, which is the captive-portal case: on a network
                // with a login page, the first bytes of the answer are a `<!DOCTYPE`.
                error!("what answered was not HTTP: {:?}", e);

                close(stream).await;

                say(Reply::not_the_api());

                return;
            }
            Ok(Ok(read)) => read,
        };

    // `ConnectionType::Close` is what the head above asked for, and it is what the answer is resolved
    // against: a server that answers `Keep-Alive` to a `close` is a mismatch, and refusing it here
    // rather than reading a body whose end nothing declared is the difference between a sentence and
    // a hang.
    //
    // The error parameter is the *socket's* error type rather than an erased one, so a caller who
    // wanted to know what MbedTLS said about a malformed head could still get at it. This function
    // does not, because the sentence is the same either way and the two are told apart by which of
    // them produced the failure.
    let body_type = match answer.resolve::<tls::Error>(ConnectionType::Close) {
        Ok((_, body_type)) => body_type,
        Err(e) => {
            error!("the API's answer does not make sense as HTTP: {:?}", e);

            close(stream).await;

            return;
        }
    };

    let mut collected = [0; BODY_REPLY_LEN];
    let mut total = 0;

    {
        // Scoped so the body reader — which borrows the stream — is gone before [`close`] needs it.
        let mut reader = Body::new(body_type, head_read.0, head_read.1, &mut *stream);

        loop {
            match with_timeout(TIMEOUT, reader.read(&mut collected[total..])).await {
                Err(_) => {
                    error!(
                        "the API stopped answering after {} of {} bytes",
                        total,
                        collected.len()
                    );

                    break;
                }
                // Zero bytes is the end of the body rather than a failure, and for a `Connection:
                // close` reply with no `Content-Length` it is the *only* way to learn where the body
                // stopped: nothing declared a length, so nothing else says.
                Ok(Ok(0)) => break,
                Ok(Ok(got)) => {
                    total += got;

                    if total == collected.len() {
                        // The buffer is full with the body still going, which is only possible for a
                        // body whose length was never declared. Said rather than passed off as
                        // complete: a Cloudflare error page cut at 256 bytes and one cut at 255 read
                        // the same otherwise.
                        warn!(
                            "the API's answer is longer than {} bytes and was cut",
                            collected.len()
                        );

                        break;
                    }
                }
                Ok(Err(e)) => {
                    error!("the API's answer could not be read: {:?}", e);

                    break;
                }
            }
        }
    }

    // Finished asking, once whatever was coming has come — TLS `close_notify`, which is the
    // protocol's own way of saying there is no more of this exchange. On every path rather than only
    // the happy one: MbedTLS warns on a session dropped while still open, and a warning that fires
    // on every failed exchange would be a warning about the reporting, not about the failure.
    close(stream).await;

    say(Reply::answered(answer.code, &collected[..total]));
}

/// Closes the connection politely, and says when it could not.
///
/// Its own function because it is on the failure paths as well as the happy one, and a closure
/// repeated at five call sites would be five chances to word the same event five different ways.
async fn close(stream: &mut tls::Stream<'_>) {
    match with_timeout(TIMEOUT, stream.close(Close::Write)).await {
        Err(_) => warn!("the exchange was not closed politely: it timed out"),
        Ok(Err(e)) => warn!("the exchange was not closed politely: {:?}", e),
        Ok(Ok(())) => {}
    }
}

/// Says what the API answered, which is the whole of what this file is for.
///
/// Takes a [`Reply`] rather than a status and a body so that the two ways of not having an answer —
/// nothing came back, and something that was not the API — are values the caller constructs rather
/// than a shape this function has to be told about.
fn say(reply: Reply<'_>) {
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

/// The address of [`HOST`], once DHCP has given the resolver some to ask.
///
/// The lookup itself is [`crate::dns::resolve`], which this shares with `src/ntp.rs`; the sentences
/// are here because a name that does not resolve means something different for an API than for a
/// time server, and a log line about a name is not answerable without the name.
async fn resolve(stack: &Stack<'static>) -> Result<Ipv4Addr, ()> {
    match crate::dns::resolve(stack, HOST).await {
        Ok(address) => Ok(address),
        Err(crate::dns::Failure::TimedOut) => {
            error!("the name of the API did not resolve in time: {}", HOST);

            Err(())
        }
        Err(crate::dns::Failure::Refused(e)) => {
            error!("the name of the API did not resolve: {} ({:?})", HOST, e);

            Err(())
        }
        Err(crate::dns::Failure::NoIpv4) => {
            error!("the name of the API is not one this can send to: {}", HOST);

            Err(())
        }
    }
}

/// Writes this chip's name: the chip, a dash, and the MAC address in hex.
///
/// Six bytes in and twelve out, so [`ID_LEN`] is twice what is needed and there is room for the chip
/// name and the dash. A value rather than a `&str` into a buffer of the caller's, because
/// `heapless::String` is already what the body is rendered into — one way of building text on this
/// chip rather than two.
fn write_id(mac: esp_hal::efuse::MacAddress) -> heapless::String<ID_LEN> {
    let mut id = heapless::String::new();

    // The `.ok()`s rather than an `unwrap`: a `fmt::Write` into a fixed string fails when it is full,
    // and this one is sized from what goes into it — twelve hex digits and at most seven for the chip
    // name, in a string of thirty-two. If that ever stops being true the id is written as far as it
    // went, which is visible in the API's table, rather than a panic in a network task.
    write!(id, "{CHIP}-").ok();

    for byte in mac.as_bytes() {
        write!(id, "{byte:02x}").ok();
    }

    id
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

/// Formats one value into a [`heapless::String`], and says what it took.
///
/// `None` when the value does not fit, which is a report that is not sent. A function rather than an
/// inline `write!` because that failure is worth one line in the log naming the capacity, and
/// `core::fmt::Error` says nothing of the sort.
fn fill<const N: usize>(value: &impl core::fmt::Display) -> Option<heapless::String<N>> {
    let mut rendered = heapless::String::new();

    match write!(rendered, "{value}") {
        Ok(()) => Some(rendered),
        Err(e) => {
            error!("a {} byte buffer was too small for this report: {:?}", N, e);

            None
        }
    }
}
