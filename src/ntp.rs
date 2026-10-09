//! Ask a time server what time it is, and set the chip's clock from the answer.
//!
//! SNTP is a request and a reply over UDP port 123, and the reply is a 48-byte header that says
//! what time the server thinks it is. There is no library for it here on purpose: the whole protocol
//! as a plain client speaks it is a name to resolve, a socket, and 48 bytes in each direction, and
//! the parts of it that are worth checking — the offsets, the 1900 epoch, a reply that is not an
//! answer to this request — are all decisions that a host can make and a board cannot test. Those
//! are in `poc-report`; this file is the part that needs a network.
//!
//! The clock this sets does not survive a power cycle, because there is nowhere on this chip to put
//! it that would. That is why the task keeps going after the first answer rather than stopping:
//! every boot starts at zero again and has to ask again.
//!
//! A failed attempt is not only logged: it is also handed to [`clock::report_failure`], which is what
//! the greeting's state line reads to explain a time that is still counting from boot. The words for
//! it live in `poc-report` with the rest of the sentences, and are tested on the host; what the
//! driver said underneath stays in this file's log.

use core::net::Ipv4Addr;

use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_net::udp::{PacketMetadata, RecvError, UdpSocket};
use embassy_net::{IpAddress, Stack};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use poc_report::{
    Answer, Clock, Obstruction, SNTP_LEN, STALE_AFTER_SECS, sntp_reply, sntp_request,
};

use crate::TIMEOUT;
use crate::clock;

/// The server to ask. `pool.ntp.org` is the pool the RFC's own examples use: anycast, so the
/// address that comes back is one of many and any of them will do.
///
/// Overridable at build time, because a network that does not reach the pool is common and the
/// alternative to an override is a firmware that cannot be pointed at a server on the local network.
const SERVER: &str = match option_env!("NTP_SERVER") {
    Some(server) => server,
    None => "pool.ntp.org",
};

/// Port 123, which is the only port an NTP server answers on, and the only one this sends to.
const PORT: u16 = 123;

/// How long to wait before asking again after a server has answered.
///
/// Once an hour, and it is [`STALE_AFTER_SECS`] rather than a number written here: that is how old an
/// answer has to be before the state line stops calling the time it set current, and a resync
/// interval longer than that would leave the clock calling itself confirmed when nothing has
/// confirmed it. Frequent enough that a chip left running overnight is still within a second or two
/// by morning, and rare enough that a proof of concept is not talking to the pool more than it
/// needs to.
const RESYNC: Duration = Duration::from_secs(STALE_AFTER_SECS);

/// How long to wait before asking again after one did not.
///
/// Fifteen seconds, so a server that is merely slow is retried while the person watching the log is
/// still watching it. Every step above has its own timeout, so this is the floor on how fast a
/// broken network can be asked in a loop.
const RETRY: Duration = Duration::from_secs(15);

/// Receive slots for the socket's metadata. One is enough for the one server this talks to, and the
/// second costs a few bytes.
const SLOTS: usize = 2;

/// The receive buffer, which has to be larger than [`SNTP_LEN`].
///
/// A server is allowed to answer with more than a bare header — extension fields, or the
/// authentication of a newer specification — and `embassy_net` drops a datagram that does not fit
/// rather than truncating it. A buffer the size of the header would therefore throw away exactly
/// the replies this cannot do anything with, and 256 bytes holds every packet a plain server sends.
const RX_LEN: usize = 256;

/// Keeps the chip's clock within a drift of real time, for as long as the network is up.
///
/// Nothing is returned and nothing is waited for: the whole client is one task, spawned from
/// `main` once the network stack exists, and every attempt inside it is bounded by a timeout of its
/// own. A network that never comes up costs this task nothing but its own waiting.
///
/// # Panics
///
/// If the executor has no room left for another task. It is allocated once, at boot, so this is a
/// fact about the size of the task pool rather than something that can happen later.
pub fn sync(spawner: Spawner, stack: Stack<'static>) {
    // Opened here rather than inside the task below. `make_static!` builds its type out of
    // `impl Trait`, and a task's body is itself an opaque type to the compiler, so one inside the
    // other is a cycle it cannot resolve (`error[E0391]`). A plain function has no such future.
    let socket = open(stack);

    spawner.spawn(keep_in_time(stack, socket).expect("keep_in_time is a task"));
}

/// Opens the one socket this client uses.
///
/// The buffers are statics behind `make_static!` because the socket borrows them for as long as it
/// lives, and it is opened once for the whole life of the firmware: `embassy_net` has a fixed number
/// of sockets and a socket set that is full panics rather than refusing, so a socket opened per
/// attempt would be a client that works once and then stops.
fn open(stack: Stack<'static>) -> UdpSocket<'static> {
    let mut socket = UdpSocket::new(
        stack,
        static_cell::make_static!([PacketMetadata::EMPTY; SLOTS]),
        static_cell::make_static!([0; RX_LEN]),
        static_cell::make_static!([PacketMetadata::EMPTY; SLOTS]),
        static_cell::make_static!([0; SNTP_LEN]),
    );

    // Bound once, to an ephemeral port, because a socket that has never been bound cannot send: the
    // port it would send from is the one the reply has to come back to, and zero is not a port.
    // `0` is the stack's way of saying "any free port", which is also what every NTP client wants —
    // the server answers to wherever the request came from rather than to a fixed port.
    socket
        .bind(0)
        .expect("a socket that was just created can always be bound");

    socket
}

/// The task behind [`sync`].
#[embassy_executor::task]
async fn keep_in_time(stack: Stack<'static>, mut socket: UdpSocket<'static>) {
    // DHCP has to finish before there is a DNS server to ask and an address to answer from.
    //
    // The servers that came with the DHCP lease are pushed into the resolver on the stack's next
    // pass rather than this one, so the first attempt can lose the race with that and fail to
    // resolve a name it would resolve a moment later. The retry below is what covers it, and the
    // reason it is logged is what makes that visible rather than mysterious.
    stack.wait_config_up().await;

    loop {
        let wait = match ask(&stack, &mut socket).await {
            Ok(answer) => {
                // The stratum comes with the time rather than being logged beside it: the state line
                // reports where the clock was set from, and an answer published without it is an
                // answer published half of.
                clock::set(answer.epoch_secs, answer.stratum);

                info!(
                    "the clock is set to {} by a stratum {} server",
                    defmt::Display2Format(&Clock::utc(answer.epoch_secs)),
                    answer.stratum,
                );

                RESYNC
            }
            Err(obstruction) => {
                error!(
                    "the time server did not answer: {}",
                    defmt::Display2Format(&obstruction)
                );

                // Also given to the clock, which is what keeps saying why the time on the greeting is
                // not a real one. It is published whether or not anybody is watching this log: the
                // reader who is not watching is the one the state line is for.
                clock::report_failure(obstruction);

                RETRY
            }
        };

        Timer::after(wait).await;
    }
}

/// One exchange: resolve the name, send the question, read the answer, and make sense of it.
async fn ask(
    stack: &Stack<'static>,
    socket: &mut UdpSocket<'static>,
) -> Result<Answer, Obstruction> {
    let server = resolve(stack).await?;

    // The nonce is the one piece of this exchange the chip can produce without a clock, and it is
    // the only thing that tells this reply from a packet that was already in flight. Saturating
    // after 49 days costs nothing: a nonce that repeats is a replay this device would have to be
    // running for weeks with a captured packet to pull off.
    let nonce = u32::try_from(Instant::now().as_millis()).unwrap_or(u32::MAX);
    let request = sntp_request(nonce);

    match with_timeout(TIMEOUT, socket.send_to(&request, (server, PORT))).await {
        Err(_) => return Err(Obstruction::RequestTimedOut),
        // The driver's own words are logged here rather than carried into the state line: a state
        // line is printed twice a second for as long as this keeps failing, and what went wrong with
        // a socket belongs in the log next to the error rather than in the sentence forever.
        Ok(Err(e)) => {
            error!("the socket would not send the request: {:?}", e);

            return Err(Obstruction::WouldNotSend);
        }
        Ok(Ok(())) => {}
    }

    let mut reply = [0; RX_LEN];
    let (read, from) = match with_timeout(TIMEOUT, socket.recv_from(&mut reply)).await {
        Err(_) => return Err(Obstruction::AnswerTimedOut),
        Ok(Err(RecvError::Truncated)) => return Err(Obstruction::TooLong),
        Ok(Ok(answer)) => answer,
    };

    // The only check the reply's own contents cannot make: anything on the local network can send
    // UDP to an open port, and a time is worth taking seriously enough to want the address checked
    // as well as the echo.
    if from.endpoint.addr != IpAddress::Ipv4(server) {
        return Err(Obstruction::Stranger);
    }

    sntp_reply(&reply[..read], nonce).map_err(Obstruction::Refused)
}

/// The address of [`SERVER`], once DHCP has given the resolver some to ask.
///
/// The lookup itself is [`crate::dns::resolve`], which this shares with `src/report.rs`; the mapping
/// onto an [`Obstruction`] is here because only this client knows which of its failures a refusal is.
async fn resolve(stack: &Stack<'static>) -> Result<Ipv4Addr, Obstruction> {
    match crate::dns::resolve(stack, SERVER).await {
        Ok(address) => Ok(address),
        Err(crate::dns::Failure::TimedOut) => Err(Obstruction::LookupTimedOut),
        Err(crate::dns::Failure::Refused(e)) => {
            error!("the name of the time server did not resolve: {:?}", e);

            Err(Obstruction::NoServer)
        }
        Err(crate::dns::Failure::NoIpv4) => Err(Obstruction::NoServer),
    }
}
