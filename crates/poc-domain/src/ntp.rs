//! What an SNTP packet off the network means, and what getting one came to when it did not.
//!
//! A packet is untrusted input, and a 48-byte header of offsets is exactly the sort of thing that is
//! right in the common case and wrong in 2036. Every check below is a decision a host can make and a
//! board cannot, which is why it is here rather than in `src/ntp.rs` — that file has the socket and
//! the timer, and needs a chip.
//!
//! [`Obstruction`] is here rather than beside the rest because it is what the refusals and the time
//! out of the socket both turn into: one vocabulary of "why there is no time yet", whatever went
//! wrong underneath it.

use core::fmt;

/// The size of an SNTP packet: the 48-byte header of RFC 5905, with no extension fields and no
/// authentication. A plain client sends exactly this much and a plain server answers with the same
/// amount, which is why the receive buffer in `src/ntp.rs` is larger rather than equal: a server is
/// allowed to send more, and a buffer that is exactly the header length drops the whole datagram.
pub const SNTP_LEN: usize = 48;

/// The leap indicator, the protocol version and the mode, packed into the first byte of the header.
const LI_VN_MODE: usize = 0;

/// How many steps the server is from a reference clock, at offset one.
const STRATUM: usize = 1;

/// Where a client puts its own timestamp, and where a server puts the time it is answering with.
const TRANSMIT: usize = 40;

/// Where a server echoes the client's timestamp back at it.
const ORIGINATE: usize = 24;

const MODE_CLIENT: u8 = 3;

const MODE_SERVER: u8 = 4;

const VERSION_4: u8 = 4;

/// Seconds between the two epochs: 1900-01-01T00:00:00Z is 2 208 988 800 seconds before
/// 1970-01-01T00:00:00Z, and an NTP timestamp counts from the first of those.
const NTP_TO_UNIX: u64 = 2_208_988_800;

/// The 48 bytes that ask a server what time it is.
///
/// `nonce` goes in the low half of the transmit timestamp and nowhere else. It is not a time — the
/// chip has no time yet, which is the whole reason for the exchange — it is a value the server has
/// to echo, so that a packet which did not answer *this* request can be told apart from one that
/// did. See [`sntp_reply`].
#[must_use]
pub fn sntp_request(nonce: u32) -> [u8; SNTP_LEN] {
    let mut packet = [0; SNTP_LEN];

    // Version 4 and mode 3. The leap indicator stays at zero, which is the honest value for a
    // client that has no clock: there is no leap second to warn about, because there is no time.
    packet[LI_VN_MODE] = VERSION_4 << 3 | MODE_CLIENT;
    packet[TRANSMIT + 4..TRANSMIT + 8].copy_from_slice(&nonce.to_be_bytes());

    packet
}

/// What a time server said, once its packet has been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    /// Seconds since the Unix epoch, which is 1970-01-01T00:00:00Z.
    ///
    /// Not adjusted, and the reason is worth writing down: NTP counts in a timescale that includes
    /// leap seconds and the Unix epoch does not, so the two disagree by however many leap seconds
    /// have been inserted since 1972. This firmware prints what the wire says rather than guessing at
    /// the correction, because a firmware that subtracts a number it read somewhere ages badly: the
    /// next leap second makes it wrong, and nothing here would notice.
    pub epoch_secs: u64,

    /// How many steps the server is from a reference clock: one is a clock that is itself a
    /// reference, such as an atomic clock or a GPS receiver, and each step above that is a machine
    /// that took its time from one of those.
    ///
    /// There is no useful threshold to compare this against, and a firmware that invented one would
    /// be guessing. What it is good for is noticing that it changed.
    pub stratum: u8,
}

/// Why an SNTP packet could not be turned into a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Fewer bytes than a header, so there is nothing to read.
    Short,

    /// The mode says this is not a reply to a client.
    NotAReply,

    /// The version is not one this client speaks.
    Version,

    /// Stratum 0, which is not a server that cannot answer: it is a server explaining why it will
    /// not, in four characters of ASCII at offset twelve.
    KissOfDeath,

    /// A stratum above 15, which is not a stratum in any version of the protocol.
    NotAServer,

    /// The leap indicator says the server does not consider itself synchronized to anything.
    Unsynchronized,

    /// The packet does not echo the nonce that was sent, so it answers some other request or
    /// repeats one that was already answered.
    NotOurs,

    /// The timestamp is zero or all ones, which is how a server writes down that it has no time.
    NoTime,

    /// The time is before 1970, so there is no epoch to count it from.
    BeforeTheEpoch,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Short => "the packet is shorter than an SNTP header",
            Self::NotAReply => "the packet is not a reply to a client",
            Self::Version => "the packet is a version of SNTP this client does not speak",
            Self::KissOfDeath => "the server will not answer this client",
            Self::NotAServer => "the packet names a stratum no version of SNTP has",
            Self::Unsynchronized => "the server says it is not synchronized to anything",
            Self::NotOurs => "the packet is not an answer to this client's request",
            Self::NoTime => "the server's packet carries no time",
            Self::BeforeTheEpoch => {
                "the server's time is before 1970, so there is no epoch to count it from"
            }
        })
    }
}

/// Reads a reply to [`sntp_request`] and says what time it claims.
///
/// The checks run in the order a server would fail them, and each is a [`Refusal`] with its own
/// sentence rather than a number: a packet off the network is untrusted input, and "the stratum is
/// 200" does not answer "why". A refusal is not an error to recover from — the caller tries another
/// server — which is why a wrong guess about which rule was broken is worse than admitting one was.
///
/// `nonce` must be the one that went into the request. Every reply that arrives on an open UDP port
/// is checked against it, because the only thing that distinguishes this server's answer to this
/// device's question from a packet that was already in flight is that it echoes what was sent.
///
/// # Errors
///
/// A [`Refusal`], which is one of three things: the packet is not a reply at all, the reply is one
/// this client cannot use — the wrong version, a stratum that means no, a server that says its own
/// clock is unsynchronized, or a timestamp that is not a time — or the reply answers some other
/// request. The caller has nothing to do with the packet in any of those cases but wait and ask
/// again, which is why the refusal is a named reason rather than an error to inspect.
pub fn sntp_reply(packet: &[u8], nonce: u32) -> Result<Answer, Refusal> {
    let header = packet.get(..SNTP_LEN).ok_or(Refusal::Short)?;

    let first = header[LI_VN_MODE];

    if first & 0b0000_0111 != MODE_SERVER {
        return Err(Refusal::NotAReply);
    }

    if first >> 3 & 0b0000_0111 != VERSION_4 {
        return Err(Refusal::Version);
    }

    // Stratum 1 is a reference clock and 15 is the furthest a server may be from one. Anything
    // outside that range is either a refusal in its own right or not a stratum at all.
    let stratum = header[STRATUM];
    match stratum {
        0 => return Err(Refusal::KissOfDeath),
        1..=15 => {}
        _ => return Err(Refusal::NotAServer),
    }

    // The two bits above the version say whether the server considers its own clock sound, and a
    // server that says it does not is not a source of time however plausible its packet looks. The
    // other two values are read and not acted on: a leap second is a repeated or skipped second at
    // the end of a UTC day, and a device whose use for its time is printing it notices neither.
    if first >> 6 == 0b11 {
        return Err(Refusal::Unsynchronized);
    }

    let request = sntp_request(nonce);
    if header[ORIGINATE..ORIGINATE + 8] != request[TRANSMIT..TRANSMIT + 8] {
        return Err(Refusal::NotOurs);
    }

    // Half of a 64-bit timestamp is a fraction and not a count of seconds. The layout is 32 bits of
    // whole seconds since 1900 followed by 32 bits of the fraction of a second, and reading all
    // eight bytes as one number multiplies the seconds by 2^32 — which on a real board turns
    // 2026-10-04 into the year 544426464172.
    //
    // The fraction is dropped rather than rounded: the reading is then the second the server was in,
    // and a rounding rule that could go either way is not worth having in a clock that prints whole
    // seconds anyway. The cost is up to one second behind the server.
    let seconds = u32::from_be_bytes([
        header[TRANSMIT],
        header[TRANSMIT + 1],
        header[TRANSMIT + 2],
        header[TRANSMIT + 3],
    ]);

    // Two ways of saying there is no time here. A zero is what the RFC defines as unknown or
    // unsynchronized, and an all-ones seconds field is what several implementations send for a clock
    // they have never set — which is also the last second era 0 can express, so neither reading is a
    // time worth setting a clock from either way.
    if seconds == 0 || seconds == u32::MAX {
        return Err(Refusal::NoTime);
    }

    // The era is not in the packet. RFC 5905 gives the 128-bit *date* format a 32-bit era number, but
    // the header's 64-bit timestamp is era-relative: era 0 counts from 1900-01-01 and its seconds
    // field uses all 32 bits — a 2026 date is past 2^31, so the top bit is set and is not a flag of
    // anything. Era 0 ends on 2036-02-07, and this reads era 0, which is the only era it can: the
    // RFC's own rule is that a client already set within 68 years of its server is right even across
    // the boundary, and a chip with no clock is not set within 68 years of anything.
    let epoch_secs = u64::from(seconds)
        .checked_sub(NTP_TO_UNIX)
        .ok_or(Refusal::BeforeTheEpoch)?;

    Ok(Answer {
        epoch_secs,
        stratum,
    })
}

/// What stood between the chip and a time, in the one attempt that failed.
///
/// A sentence rather than a variant of somebody else's error type, because this is what ends up in
/// the serial log and in the state line. The errors this stands for live in three crates — the
/// resolver, the socket and [`Refusal`] — and each of them answers "what happened" without saying
/// what to do about it, which is the question the state line exists to answer.
///
/// The three timeouts are three variants rather than one carrying a name for the step, for the same
/// reason the words are here at all: a name in a payload is prose written where the error happened,
/// and prose written there is prose nothing checks. A step that never finished is the difference
/// between a name that does not resolve and a server that does not answer, and those want opposite
/// fixes — the first is a DNS or a network problem, the second is a server or a firewall.
///
/// Published as the value itself for the task that keeps the clock to read: what the one attempt
/// that failed came to, written once per attempt and read until the next one. `None` before the
/// first attempt rather than an explanation of nothing having gone wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Obstruction {
    /// The server's name did not resolve before the exchange gave up on it.
    LookupTimedOut,

    /// The request did not reach the server.
    RequestTimedOut,

    /// The server's answer did not arrive.
    AnswerTimedOut,

    /// The server's name resolved to nothing this firmware can send to.
    NoServer,

    /// A reply came from an address that was not the one that was asked.
    Stranger,

    /// The socket would not send the request at all.
    WouldNotSend,

    /// A reply arrived and did not fit the receive buffer.
    TooLong,

    /// A reply arrived and said something this client cannot use, which [`Refusal`] has already
    /// turned into a sentence.
    Refused(Refusal),
}

impl fmt::Display for Obstruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Every step named, because "did not finish in time" alone sends whoever is reading it
            // after the network, and a name that does not resolve and a server that does not answer
            // are not the same problem.
            Self::LookupTimedOut => {
                f.write_str("the name of the time server did not resolve in time")
            }
            Self::RequestTimedOut => f.write_str("the request did not reach the time server"),
            Self::AnswerTimedOut => f.write_str("the time server's answer did not arrive"),
            Self::NoServer => {
                f.write_str("the name of the time server did not resolve to an address")
            }
            Self::Stranger => f.write_str("the reply came from an address that was not asked"),
            Self::WouldNotSend => f.write_str("the socket would not send the request"),
            Self::TooLong => f.write_str("the reply was larger than the receive buffer"),
            Self::Refused(refused) => write!(f, "{refused}"),
        }
    }
}
