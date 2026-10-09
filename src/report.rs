//! Report what this chip is doing to an HTTPS API, every five minutes, over TCP.
//!
//! This is the first thing in the firmware that talks to something outside the local network: an
//! answer can be refused, reset mid-exchange, or not be the API at all.
//!
//! **The HTTP framing is `edge-http`'s** — connect, request head, and the loop that reads until a
//! reply is whole. That is where the fiddly part of an HTTP client lives, and this firmware shipped
//! the bug it caused. What is left here is the one thing a general-purpose client cannot decide:
//! **which API this build reports to**, and what to say about what it said back.
//!
//! **It sends no credentials**, so the API answers 401 and the log says so on every pass — that line
//! is the feature working, not a bug. The exchange is over TLS through [`crate::tls`], which is
//! what makes the token the next piece of work safe to add.

use core::ffi::CStr;
use core::fmt::Write as _;
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use defmt::{error, info, warn};
use edge_http::Method;
use edge_http::io::Body;
use edge_http::io::client::Connection;
use edge_nal::io::{Read, Write};
use edge_nal_embassy::Tcp as EmbassyTcp;
use edge_nal_tls::TlsConnector;
use embassy_executor::Spawner;
use embassy_net::Stack;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_hal::peripherals::{ADC1, RNG};
use poc_report::{EVENT_TELEMETRY, EVENTS_PATH, Event, REPORT_EVERY_SECS, Time};

use crate::{TIMEOUT, clock, status, tls};

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

/// How long the connect, the handshake and the request head may take together.
///
/// One budget rather than three because `edge-http`'s `Connection` does all three inside one call:
/// the TCP connect, the TLS handshake and the head going out are a single `initiate_request`, so a
/// line saying "the handshake failed" and a line saying "the head did not go out" would be two names
/// for one timeout. Every other step is [`crate::TIMEOUT`], which is the connect included: a
/// connection that could not be established is a step that did not finish in time, like any other.
///
/// The handshake is what makes this longer than [`crate::TIMEOUT`]: every other step is a request
/// going out or a reply coming back over an established connection, while this one is the API
/// proving who it is, which on a 160 MHz RISC-V running `MbedTLS`' own arithmetic rather than the
/// chip's accelerators is seconds rather than milliseconds. The handshake is also renegotiated from
/// scratch every five minutes, because the connection is built fresh each time and paid for in full
/// every time.
///
/// Twenty seconds is a ceiling, not a measurement: the observed handshake on the C3 completes well
/// inside five seconds, so this only bounds the failure where nothing comes back at all.
const HANDSHAKE: Duration = Duration::from_secs(20);

/// How many bytes of body to build.
///
/// 512, comfortably over the longest body this firmware can produce: a failed join with a signal in it
/// and a clock counting from boot, which `poc-report`'s tests measure at under 300. The buffer is
/// sized over the worst case rather than the usual one, because a body that does not fit is a report
/// that is silently not sent.
const BODY_LEN: usize = 512;

/// How many headers this exchange will accept back.
///
/// `edge-http`'s `Headers` is `[httparse::Header; N]` held **by value**, so `N` is stack rather than
/// heap: its default of 64 is about a kilobyte of the reporting task's stack for no benefit, since
/// the deployed Worker answers with ten.
///
/// Sixteen rather than ten because `Headers::set` **panics** with `No space left` rather than
/// returning an error, so the count is a promise the type system does not check. The request headers
/// are a slice of tuples and grow without one — the `Authorization` header that is the next piece of
/// work is one more tuple, not another visit to this constant.
const HEADERS: usize = 16;

/// How many bytes of scratch `Content-Length` is rendered into.
///
/// The length goes out as one of the header tuples, and a header value is text while the number is
/// not one yet. Twenty is a `u64` in full: more than any length this body can have, and the type
/// says so.
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

/// How many bytes of state line to build.
///
/// 256, comfortably over the longest line this firmware can produce: a clock counting from boot
/// with a failure to explain, an address, and a failed join with a signal in it. The line is
/// rendered before the event because the payload is what the chip would have printed, and a line
/// that does not fit is a report that is not sent.
const STATUS_LEN: usize = 256;

/// Starts reporting, and returns once the task is spawned.
///
/// Nothing is returned and nothing is waited for: the whole reporter is one task, spawned from `main`
/// once the network stack exists, and every step inside it is bounded by a timeout of its own. A
/// network that never comes up costs this task nothing but its own waiting.
///
/// `rng` and `adc` are the two halves of the entropy `MbedTLS` needs for its key exchange: the
/// generator is only handed out once the SAR ADC source behind it has been enabled, which takes
/// both peripherals. They travel by value because the task enables the source itself, once DHCP is
/// up — not here, where nothing has joined yet. See [`crate::tls::boot`] for why the wait matters.
///
/// # Panics
///
/// If the executor has no room left for another task. It is allocated once, at boot, so this is a
/// fact about the size of the task pool rather than something that can happen later.
pub fn start(spawner: Spawner, stack: Stack<'static>, rng: RNG<'static>, adc: ADC1<'static>) {
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
    // opaque type to the compiler, so one inside the other is a cycle it cannot resolve. This one
    // is storage rather than state — and building it touches no hardware, so it costs the radio
    // nothing.
    let name = server_name(static_cell::make_static!([0; NAME_LEN]));

    spawner.spawn(report(stack, rng, adc, name).expect("report is a task"));
}

/// The task behind [`start`].
#[embassy_executor::task]
async fn report(stack: Stack<'static>, rng: RNG<'static>, adc: ADC1<'static>, name: &'static CStr) {
    // The first report waits for DHCP rather than firing into a stack that has no address yet: the
    // exchange needs an address to send from, and the resolver it needs to find the API with is
    // configured by the same lease.
    stack.wait_config_up().await;

    let connector = tls::boot(rng, adc, stack, name);

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

    // Rendered before the event because the payload is what the chip would have printed at that
    // moment rather than the three fields behind it: three fields in a payload would be a second
    // way of writing the same line, and the two would drift.
    let Some(payload) = fill::<STATUS_LEN>(&state) else {
        return;
    };

    let id = write_id(esp_hal::efuse::base_mac_address());

    let event = Event {
        device_id: id.as_str(),
        timestamp_secs: now_secs(),
        event_type: EVENT_TELEMETRY,
        payload: payload.as_str(),
    };

    // The body is built before the head because `Content-Length` is its length and the head goes
    // first on the wire. A body that does not fit is a report that is not sent, and [`fill`] says so
    // in the log rather than sending a truncated one.
    let Some(body) = fill::<BODY_LEN>(&event) else {
        return;
    };

    // The scratch `Content-Length` is rendered into outlives the headers borrowing from it: the
    // tuples below hold a `&str` each, so the string they borrow lives here rather than inside
    // whatever builds them.
    let mut length = heapless::String::<LENGTH_LEN>::new();
    write!(length, "{}", body.len()).ok();

    // Four headers, and each is here for a reason rather than by habit. `Host` is the name this
    // firmware asked for rather than the address it resolved to, because that is what a server routes
    // on and what a log can be read against. `Content-Length` is measured rather than declared,
    // because the body was written into a buffer before any of this and a length that disagrees with
    // it is a request the server waits on until it gives up. `Connection: close` because this client
    // reads the answer and does nothing else with the connection: without it the server may hold the
    // socket open, and the read below then waits for bytes that are not coming, which is a timeout
    // on every single report rather than once.
    let headers = [
        ("Host", HOST),
        ("Content-Type", "application/json"),
        ("Content-Length", length.as_str()),
        ("Connection", "close"),
    ];

    // The scratch the exchange runs in: `Connection` parses the answer's head out of it and hands
    // back what arrived past the blank line as the start of the body. About a kilobyte of stack for
    // the life of one exchange, freed on the way out rather than held across five-minute sleeps.
    let mut scratch = [0; tls::RX_LEN];
    let mut connection = Connection::new(
        &mut scratch,
        connector,
        SocketAddr::new(IpAddr::V4(address), port()),
    );

    // Connect, shake hands and send the head in one call, under the handshake's own budget: the
    // handshake is seconds of `MbedTLS` arithmetic on this chip rather than milliseconds of
    // network, and twenty seconds bounds only the failure where nothing comes back at all. Each
    // step below returns having already said what went wrong, so the caller only has to stop.
    //
    // Nothing here has to arrange for the connection to be reusable: the factory builds the socket
    // out of a pool and the connection is dropped on the way out whatever the outcome, which is
    // also what a handshake that timed out needs — `smoltcp` refuses the next `connect` on a socket
    // that is still open as an invalid state.
    match with_timeout(
        HANDSHAKE,
        connection.initiate_request(true, Method::Post, EVENTS_PATH, &headers),
    )
    .await
    {
        Err(_) => {
            error!("the request did not go out: it timed out");

            return;
        }
        Ok(Err(e)) => {
            error!("the request did not go out: {:?}", e);

            return;
        }
        Ok(Ok(())) => {}
    }

    // The version and the verification flags together are what says the API was checked and not
    // merely reached: a zero flag is the only value that means the chain, the signature and the
    // hostname all agreed, and the version says what was agreed about. The handshake is already
    // over — the head above went out through it — so this reads what it settled on rather than
    // driving it.
    match connection.raw_connection() {
        Err(e) => {
            error!("the handshake could not be read back: {:?}", e);

            return;
        }
        Ok(socket) => {
            let session = socket.session_mut();

            info!(
                "the API's certificate verified: {:?}, flags {:#x}",
                session.tls_version(),
                session.tls_verification_details()
            );
        }
    }

    match with_timeout(TIMEOUT, connection.write_all(body.as_bytes())).await {
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

    match with_timeout(TIMEOUT, connection.initiate_response()).await {
        Err(_) => {
            error!("the API did not finish answering in time");

            close(connection).await;

            return;
        }
        Ok(Err(edge_http::io::Error::ConnectionClosed)) => {
            // Nothing at all came back: a peer that accepted the connection and then stopped. That is
            // not the same as something answering that was not the API, and it wants a different
            // sentence — one names the API being unreachable, the other names a portal.
            close(connection).await;

            say(None, &[]);

            return;
        }
        Ok(Err(e)) => {
            // Something answered and it was not HTTP, which is the captive-portal case: on a network
            // with a login page, the first bytes of the answer are a `<!DOCTYPE`.
            error!("what answered was not HTTP: {:?}", e);

            close(connection).await;

            say(None, &[]);

            return;
        }
        Ok(Ok(())) => {}
    }

    let (head, reader) = connection.split();
    let code = head.code;

    let mut collected = [0; BODY_REPLY_LEN];
    let total = collect(reader, &mut collected).await;

    // Finished asking, once whatever was coming has come: closing the connection politely, which is
    // the protocol's own way of saying there is no more of this exchange. On every path rather than
    // only the happy one: MbedTLS warns on a session dropped while still open, and a warning that
    // fires on every failed exchange would be a warning about the reporting, not about the failure.
    close(connection).await;

    say(Some(code), &collected[..total]);
}

/// Reads the answer's body into `collected`, and says how many bytes it took.
///
/// Its own function rather than more of [`once`] because that function is long enough already, and
/// the loop is the one part of the exchange with a buffer of its own.
///
/// Cut rather than refused when the body does not fit, and said so in the log when it happens: a
/// body too long for this is a Cloudflare error page, and the first 120 characters of one already
/// say it is HTML.
async fn collect(
    reader: &mut Body<'_, tls::Stream<'_>>,
    collected: &mut [u8; BODY_REPLY_LEN],
) -> usize {
    let mut total = 0;

    loop {
        match with_timeout(TIMEOUT, reader.read(&mut collected[total..])).await {
            Err(_) => {
                error!(
                    "the API stopped answering after {} of {} bytes",
                    total,
                    collected.len()
                );

                return total;
            }
            // Zero bytes is the end of the body rather than a failure, and for a `Connection:
            // close` reply with no `Content-Length` it is the *only* way to learn where the body
            // stopped: nothing declared a length, so nothing else says.
            Ok(Ok(0)) => return total,
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

                    return total;
                }
            }
            Ok(Err(e)) => {
                error!("the API's answer could not be read: {:?}", e);

                return total;
            }
        }
    }
}

/// Closes the connection politely, and says when it could not.
///
/// Its own function because it is on the failure paths as well as the happy one, and a closure
/// repeated at five call sites would be five chances to word the same event five different ways.
///
/// Nothing is closed before the answer is read. The head says `Connection: close`, so the server
/// closes when it has answered, and the answer is what this is waiting for: shutting the write half
/// first measured as a `ConnectionReset` before a single byte came back, which is this server's
/// answer to being told the conversation was over before it had replied to it.
async fn close(connection: Connection<'_, TlsConnector<'static, EmbassyTcp<'static>>, HEADERS>) {
    match with_timeout(TIMEOUT, connection.close()).await {
        Err(_) => warn!("the exchange was not closed politely: it timed out"),
        Ok(Err(e)) => warn!("the exchange was not closed politely: {:?}", e),
        Ok(Ok(())) => {}
    }
}

/// Says what the API answered, which is the whole of what this file is for.
///
/// Takes the status code and the body rather than a verdict on them: 201 is the one status that
/// means the exchange worked, and every other status is reported as the number it is plus the
/// API's own words. A mapping from numbers to sentences would be a claim about what each status
/// means — tomorrow a 401 can mean a token that expired rather than one that was never sent —
///
/// `None` for the code when no status line was read at all: nothing came back, or something that
/// was not the API answered. Saying "no status" beats printing a zero, which is not a thing this
/// API can send and would read as a status.
fn say(code: Option<u16>, body: &[u8]) {
    // The status code goes on every line that has one, and it is the one thing here that is not
    // this firmware's opinion: it is what the API actually did. A reader who wants more than the
    // number has the body on the next line.
    match code {
        Some(201) => info!(
            "the API stored the event, 201 after {}",
            defmt::Display2Format(&poc_report::age(Instant::now().as_secs()))
        ),
        Some(status) => warn!("the API did not store the event: {}", status),
        None => warn!("the API did not store the event: no status"),
    }

    // The API's own words, on anything that was not a store. On a 400 this is the only thing that
    // says which field was wrong; on a status nobody named, it is the only thing the API said at
    // all. Left out for a store, whose body is eleven bytes saying "ok" — every five minutes,
    // forever, and a log line nobody reads is a log line that costs time to skip.
    if code != Some(201) {
        info!(
            "the API said: {}",
            defmt::Display2Format(&poc_report::logged(body))
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
