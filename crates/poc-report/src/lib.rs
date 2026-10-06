//! What the firmware decides for itself, decided here so that something can check it.
//!
//! Seven decisions live in this crate, and every one of them is one the firmware would otherwise get
//! subtly wrong and nobody would notice until it mattered:
//!
//! - How an address is written. The greeting in `src/bin/main.rs` and the report in `src/wifi.rs`
//!   both print one, and they used to be two pieces of formatting that could drift apart.
//! - What a failed join says. The radio names about fifty reasons, and printing its own words for
//!   them answers the question "what did the hardware say" rather than the question an operator is
//!   asking, which is what to do next.
//! - What the radio is doing, and the word that state is published in. A link that is down is
//!   something a reader of a serial log has to be told about, and it is something the radio knows
//!   and the greeting does not.
//! - How a time is written, from a count of seconds. Leap years and month lengths are arithmetic
//!   that cannot be checked by looking at it, and this is the only part of the firmware that knows
//!   what day it is.
//! - What an SNTP packet means, and what an attempt to get one came to when it did not. A packet
//!   off the network is untrusted input, and a 48-byte header of offsets is exactly the sort of
//!   thing that is right in the common case and wrong in 2036.
//! - What the line the firmware prints twice a second reads, and whether the time on it is real.
//!   That line is the whole of what this repository says about itself to whoever is reading it, and
//!   it was assembled from three `format!`s in a generated file.
//! - What the body of a reported event looks like, and what an answer from the API means. The bytes
//!   of an HTTP request are the same kind of thing as the bytes of an SNTP header: right in the
//!   common case, wrong in the detail nobody reads, and impossible to check on a board.
//!
//! It is a crate of its own because this is the only part of the firmware that can be tested at all.
//! `src/wifi.rs`, `src/clock.rs`, `src/ntp.rs`, `src/status.rs` and `src/bin/main.rs` all depend on
//! `esp-hal` or on the network stack above it, which exist only for this chip, so none of them can
//! be compiled for a host — and a test on a microcontroller needs a board or a simulator, which the
//! gate does not have. This crate is `#![no_std]` with no dependencies, so it builds for the chip
//! and for the host alike, and `tests/report.rs`, `tests/status.rs` and `tests/ntp.rs` run on
//! whichever machine is running the gate.
//!
//! It knows nothing about Wi-Fi or about the network stack. `esp_radio::wifi::DisconnectReason` is
//! translated into a [`Reason`] in `src/wifi.rs`, and an SNTP packet is read in the firmware and
//! handed here as bytes, so that a change in either driver costs that one file rather than this
//! crate's API.

#![no_std]

use core::fmt::{self, Write as _};
use core::net::Ipv4Addr;

/// An address the stack is holding, with the prefix length that goes with it.
///
/// Written the way a router writes one — `192.168.0.225/24` — because that is the form both the
/// greeting and the report print, and the prefix belongs to the address rather than to whichever of
/// them is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    /// The address itself.
    pub ip: Ipv4Addr,

    /// How many of its leading bits name the network rather than the host.
    pub prefix_len: u8,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix_len)
    }
}

/// Why joining a network did not work, in as few words as the radio allows.
///
/// The four cases below are the ones that want four different things done about them; everything else
/// the radio can report is [`Reason::Other`], which is a deliberate admission rather than a guess.
/// A wrong guess here is worse than a missing one, because it sends whoever is reading the serial
/// output after the wrong problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Nothing with that name was heard.
    ///
    /// One of three things: the name is wrong, the network is not there, or the station is too far
    /// from it to be answered. The scan printed alongside this failure is what tells those apart.
    NoSuchNetwork,

    /// The network was heard and refused how this station asked to join it.
    ///
    /// In practice the password, or a security type that WPA2 does not cover.
    SecurityRefused,

    /// The exchange started and the other end stopped answering.
    ///
    /// This is what a station too far from its access point looks like from its own side: strong
    /// enough to hear a beacon, too weak to finish a handshake.
    NoAnswer,

    /// The handshake began and did not finish, and the radio does not say why.
    ///
    /// Its own variant rather than one of the two above because both of them would be a claim the
    /// radio did not make. A four-way handshake that times out is the commonest symptom of a wrong
    /// password, and it is also what a station just out of range looks like; `SecurityRefused` says
    /// the network refused, and `NoAnswer` says it went away, and a timeout establishes neither. What
    /// the radio reported is that the exchange started and stopped, so that is what this says.
    ///
    /// Observed on a board rather than reasoned about: an ESP32-C6 that never joined logged
    /// `FourWayHandshakeTimeout` at -55 dBm, which is a strong signal and not a refused password.
    HandshakeStalled,

    /// The link came up and then went down.
    LinkLost,

    /// The radio reported something this firmware does not name.
    Other,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(write_reason(*self))
    }
}

/// A join that failed, with the signal the radio last measured.
///
/// Named for the join rather than just "a failure", because [`Obstruction`] is the other failure in
/// this crate and `Failure` alone stopped saying which of the two a use was talking about.
///
/// The signal is there because the two failures that look identical in the radio's own words are not
/// identical to fix: a wrong password and a station too far away both arrive as an exchange that
/// stops, and the dBm reading is what separates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinFailure {
    /// What went wrong.
    pub reason: Reason,

    /// How strong the signal was, in dBm, when the radio measured one.
    pub signal: Option<i8>,
}

impl fmt::Display for JoinFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.signal {
            Some(dbm) => write!(f, "{} (signal {dbm} dBm)", self.reason),
            None => fmt::Display::fmt(&self.reason, f),
        }
    }
}

/// The sentence for a reason, on its own.
///
/// It is a function rather than an arm of the `Display` above so that adding a case to the enum stays
/// one edit: [`JoinFailure`] reaches it for the arm that has no signal to add, and a match written
/// twice is a match that gets changed in one of the two places.
fn write_reason(reason: Reason) -> &'static str {
    match reason {
        Reason::NoSuchNetwork => "nothing with that name was heard",
        Reason::SecurityRefused => "the network refused these credentials",
        Reason::NoAnswer => "the network stopped answering partway through",
        Reason::HandshakeStalled => "the handshake started and did not finish",
        Reason::LinkLost => "the link came up and then went down",
        Reason::Other => "the radio reported a reason this firmware does not name",
    }
}

/// What the radio is doing, and — when it is not doing what it should — why.
///
/// A state rather than a sequence of log lines because a log is read long after the event. A line
/// saying it could not join helps only whoever is watching when it happens; a line that still says
/// so twice a second is what someone reading the log from the top finds, and to them the two are the
/// same thing.
///
/// The states are separate rather than one "not connected" because each wants a different thing done
/// about it: fill in the credentials, fix a credential, replace the board, or go and look for the
/// network and how loudly it can be heard from here.
///
/// `src/wifi.rs` publishes this and `src/status.rs` reads it. It is a published value rather than a
/// return value because the radio runs in its own task and the greeting runs in the main one, and
/// nothing either of them waits for the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// There was never a network to join, because no `SSID` was compiled in.
    ///
    /// The normal state of the gate and of CI, and the state a build made before `.cargo/local.toml`
    /// was filled in is in. Nothing is wrong with the hardware, which is what makes this worth
    /// distinguishing from every other state here.
    NoNetwork,

    /// There was a network to join and the radio would not take one of its credentials.
    ///
    /// Both values are compiled in, so this is a mistake in the file the firmware was built from
    /// rather than anything that can happen on a board.
    UnusableCredential,

    /// The radio would not start at all, which is neither a network problem nor a credentials one.
    NoRadio,

    /// An attempt is in progress, or one is due.
    Joining,

    /// The station is on the network.
    Joined,

    /// The last attempt did not work, and this is what it said.
    Failed(JoinFailure),

    /// A word this build of the firmware did not write.
    ///
    /// Not reachable from this crate — it is what [`Link::from_word`] answers for a word whose tag
    /// names no state here, which is the honest reading of a value another build published with a
    /// different idea of the layout. An explicit variant rather than an `Option` so that the reader
    /// of the greeting, which runs twice a second, has nothing to do about it either.
    Unknown,
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The same three words on the three states with no network in them, because what they
            // have in common is what a reader has to act on first: there is nothing to wait for.
            Self::NoNetwork => f.write_str("nothing to join: no credentials were compiled in"),
            Self::UnusableCredential => f.write_str(
                "nothing to join: a compiled-in credential is not one the radio can use",
            ),
            Self::NoRadio => f.write_str("nothing to join: the radio did not start"),
            Self::Joining => f.write_str("joining"),
            Self::Joined => f.write_str("joined"),
            Self::Failed(failure) => write!(f, "not joined: {failure}"),
            Self::Unknown => f.write_str("in a state this firmware does not name"),
        }
    }
}

/// The word each state is published as.
///
/// Written down rather than derived from the order of the variants, because a word is a thing that
/// two builds of this firmware have to agree about and `as u8` would make that agreement a property
/// of an enum that anyone may reorder. `Unknown` is last and not `6`, so that the tags a build does
/// not recognize are the ones in the middle.
const TAG_NO_NETWORK: u8 = 0;
const TAG_UNUSABLE: u8 = 1;
const TAG_NO_RADIO: u8 = 2;
const TAG_JOINING: u8 = 3;
const TAG_JOINED: u8 = 4;
const TAG_FAILED: u8 = 5;
const TAG_UNKNOWN: u8 = 255;

/// The byte saying that the one beside it is a measurement rather than an absence.
///
/// `0 dBm` and "no reading at all" are the same byte and not the same claim, and the radio is willing
/// to report one as though it were the other — `src/wifi.rs` maps its "no reading" of -128 to `None`
/// — so the presence of the reading is a byte of its own rather than a value the byte cannot hold.
const SIGNAL_MEASURED: u8 = 1;

/// The byte that says the signal beside it is not a reading.
const SIGNAL_NONE: u8 = 0;

impl Link {
    /// The four bytes this state is published as, lowest first.
    const fn bytes(self) -> [u8; 4] {
        match self {
            Self::NoNetwork => [TAG_NO_NETWORK, 0, 0, 0],
            Self::UnusableCredential => [TAG_UNUSABLE, 0, 0, 0],
            Self::NoRadio => [TAG_NO_RADIO, 0, 0, 0],
            Self::Joining => [TAG_JOINING, 0, 0, 0],
            Self::Joined => [TAG_JOINED, 0, 0, 0],
            Self::Unknown => [TAG_UNKNOWN, 0, 0, 0],
            Self::Failed(JoinFailure { reason, signal }) => {
                let [measured, reading] = signal_bytes(signal);

                [TAG_FAILED, reason_byte(reason), reading, measured]
            }
        }
    }

    /// The word this state is published as, for [`Self::from_word`] to read back.
    ///
    /// One word rather than several held in a fixed order, because several is more than one write
    /// and an ordering argument between them — one that a reader from another task can only get
    /// right by trusting the order two writers happened to use. A single word cannot be half
    /// written.
    ///
    /// `const` because the static in `src/wifi.rs` that holds it has to be initialized by one.
    #[must_use]
    pub const fn to_word(self) -> u32 {
        u32::from_le_bytes(self.bytes())
    }

    /// The state a published word stands for.
    ///
    /// Total rather than an `Option`: the greeting reads this twice a second, and a value it has to
    /// match on before it can print anything is a place for a match to be wrong. Every tag no build
    /// of this firmware writes — including [`Link::Unknown`]'s own — reads as [`Link::Unknown`],
    /// which is the sentence for a word that means nothing here.
    #[must_use]
    pub fn from_word(word: u32) -> Self {
        let [tag, reason, reading, measured] = word.to_le_bytes();

        match tag {
            TAG_FAILED => Self::Failed(JoinFailure {
                reason: reason_from_byte(reason),
                signal: signal_from_byte([measured, reading]),
            }),
            TAG_NO_NETWORK => Self::NoNetwork,
            TAG_UNUSABLE => Self::UnusableCredential,
            TAG_NO_RADIO => Self::NoRadio,
            TAG_JOINING => Self::Joining,
            TAG_JOINED => Self::Joined,
            _ => Self::Unknown,
        }
    }
}

/// The presence of a signal and the signal itself, as the two bytes they are published as.
///
/// The reading is read out of its own bytes rather than cast: `clippy::cast_sign_loss` is on, it is
/// right about what the cast does, and an `i8` and the `u8` holding the same eight bits are the same
/// number — there is no sign here to lose, only a number that happens to be negative.
const fn signal_bytes(signal: Option<i8>) -> [u8; 2] {
    match signal {
        Some(dbm) => {
            let [byte] = dbm.to_le_bytes();

            [SIGNAL_MEASURED, byte]
        }
        None => [SIGNAL_NONE, 0],
    }
}

/// The signal two published bytes stand for, or `None` when the first says there is none.
const fn signal_from_byte(bytes: [u8; 2]) -> Option<i8> {
    match bytes[0] {
        SIGNAL_MEASURED => {
            let [byte] = bytes[1].to_le_bytes();

            Some(i8::from_le_bytes([byte]))
        }
        _ => None,
    }
}

/// The byte a reason is published as.
///
/// A match rather than `as u8` for the reason given on [`TAG_NO_NETWORK`]: this is a wire format, and
/// a match is something a reader can check against the enum it encodes without knowing Rust.
const fn reason_byte(reason: Reason) -> u8 {
    match reason {
        Reason::NoSuchNetwork => 0,
        Reason::SecurityRefused => 1,
        Reason::NoAnswer => 2,
        // 3 and 4 are the two this one was added between: `LinkLost` and `Other` keep the bytes they
        // had. Both are within one firmware build, so renumbering them would change nothing the gate
        // can see — and a published word that means something else after an edit is not worth the
        // tidiness of consecutive numbers.
        Reason::HandshakeStalled => 5,
        Reason::LinkLost => 3,
        Reason::Other => 4,
    }
}

/// The reason a published byte stands for.
///
/// A byte no build of this firmware writes reads as [`Reason::Other`], which is the sentence the
/// enum already has for a reason this firmware does not name — and which is what
/// `src/wifi.rs` publishes for a reason added upstream, since the two are the same case.
const fn reason_from_byte(byte: u8) -> Reason {
    match byte {
        0 => Reason::NoSuchNetwork,
        1 => Reason::SecurityRefused,
        2 => Reason::NoAnswer,
        3 => Reason::LinkLost,
        5 => Reason::HandshakeStalled,
        _ => Reason::Other,
    }
}

/// Seconds in a day, which is as far as [`Clock::SinceBoot`] goes before it starts again.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// What the firmware believes the time is, and where that belief comes from.
///
/// The two variants are different in kind and the difference is the point of the type. There is no
/// battery-backed clock on this chip, so [`Self::SinceBoot`] is the only reading it can make on its
/// own: the scheduler's counter, which starts at zero when the firmware starts. A real time has to
/// come from the network, and until one has arrived the two are told apart by their shape rather
/// than by anything else, which is why [`Self::Utc`] prints a date and this one does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    /// How long the chip has been running, wrapped into a day.
    ///
    /// This is not a time of day and is not meant to read like one. It is written as `HH:MM:SS`
    /// because that is the shape a reader of a serial log is looking for, and it wraps at midnight
    /// rather than counting hours forever so that it cannot be mistaken for a clock that stopped.
    SinceBoot(u64),

    /// A time of day in UTC, as a count of seconds since the Unix epoch.
    ///
    /// UTC and nothing else: this is what a network time source hands out, and a reading that
    /// disagrees with it by a timezone has been adjusted by somebody, which is not this crate's
    /// business. The count is unsigned because a time before 1970 is not a time this can print,
    /// and an SNTP server that reports one is refused rather than rendered — see [`sntp_reply`].
    Utc(u64),
}

impl Clock {
    /// The reading the chip can make on its own, from how long it has been running.
    ///
    /// The arithmetic happens when the reading is printed rather than here, so there is no way for a
    /// half-computed time of day to exist: there is a number of seconds and nothing else.
    #[must_use]
    pub const fn since_boot(since_boot_secs: u64) -> Self {
        Self::SinceBoot(since_boot_secs)
    }

    /// A real time, from a count of seconds since 1970-01-01T00:00:00Z.
    #[must_use]
    pub const fn utc(epoch_secs: u64) -> Self {
        Self::Utc(epoch_secs)
    }
}

impl fmt::Display for Clock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SinceBoot(since_boot_secs) => {
                let today = since_boot_secs % SECONDS_PER_DAY;

                let (hours, minutes, seconds) = (today / 3600, today / 60 % 60, today % 60);

                write!(f, "{hours:02}:{minutes:02}:{seconds:02}")
            }
            // A date as well as a time: a clock that cannot say which day the hours belong to is
            // half a clock, and the date is the part that catches a reading that is a century out.
            Self::Utc(epoch_secs) => {
                let civil = civil(*epoch_secs);

                write!(
                    f,
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    civil.year, civil.month, civil.day, civil.hour, civil.minute, civil.second
                )
            }
        }
    }
}

/// A date and a time of day, as the plain numbers that are written down.
///
/// Every field is a `u64` rather than the smallest type that holds it, so that turning a count of
/// seconds into this is arithmetic and not a pile of conversions: `clippy`'s `cast_possible_truncation`
/// fires on a cast that is provably in range, and the arithmetic below is easier to check for
/// correctness than a cast is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Civil {
    year: u64,
    month: u64,
    day: u64,
    hour: u64,
    minute: u64,
    second: u64,
}

/// The proleptic Gregorian date and time of a count of seconds since the Unix epoch.
///
/// Howard Hinnant's `civil_from_days`, which is the arithmetic every calendar library ends up
/// containing: 1970-01-01 falls 719 468 days into a cycle of 146 097 days that starts on
/// 0000-03-01, a cycle is 400 years, and the year within one follows from the day within the year.
///
/// It is here rather than in the firmware because it is arithmetic that cannot be checked by
/// looking at it. Month lengths and leap years are exactly the sort of thing that is right in the
/// common cases and wrong in February, and `tests/report.rs` pins the ends of each of them.
fn civil(epoch_secs: u64) -> Civil {
    let seconds_of_day = epoch_secs % SECONDS_PER_DAY;

    // 0000-03-01 is 719 468 days after 1970-01-01, which puts the epoch inside a 400-year cycle
    // rather than at the start of one. Every count here is of a day count, and every one of them
    // is of a non-negative number: this is only ever called with seconds since 1970.
    let days = epoch_secs / SECONDS_PER_DAY + 719_468;

    let era = days / 146_097;
    let day_of_era = days % 146_097;

    // [0, 399], skipping the leap day that ends a century which is not a leap year itself.
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;

    // [0, 365]
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);

    // Counting months from March, which is what makes the leap day the last day of the year and
    // removes every special case. [0, 11]
    let month_of_year = (5 * day_of_year + 2) / 153;

    let day = day_of_year - (153 * month_of_year + 2) / 5 + 1;
    let month = if month_of_year < 10 {
        month_of_year + 3
    } else {
        month_of_year - 9
    };

    // The year of the era runs from March, so January and February belong to the year after it.
    let year = year + u64::from(month <= 2);

    Civil {
        year,
        month,
        day,
        hour: seconds_of_day / 3600,
        minute: seconds_of_day / 60 % 60,
        second: seconds_of_day % 60,
    }
}

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

/// Mode 3: a client asking for the time.
const MODE_CLIENT: u8 = 3;

/// Mode 4: a server answering.
const MODE_SERVER: u8 = 4;

/// Version 4 of the protocol, which is the version this client speaks.
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
    /// have been inserted since 1972 — 27 when this was written, and the count only goes up. This
    /// firmware prints what the wire says rather than guessing at the correction, because a firmware
    /// that subtracts 27 on the strength of a number it read somewhere ages badly: the next leap
    /// second makes it wrong, and nothing here would notice.
    pub epoch_secs: u64,

    /// How many steps the server is from a reference clock: one is a clock that is itself a
    /// reference, such as an atomic clock or a GPS receiver, and each step above that is a machine
    /// that took its time from one of those.
    ///
    /// There is no useful threshold to compare this against, and a firmware that invented one would
    /// be guessing. What it is good for is noticing that it changed.
    pub stratum: u8,

    /// What the server says about leap seconds.
    pub leap: Leap,
}

/// What a server says about leap seconds, in the two bits at the top of the packet's first byte.
///
/// Nothing is done with it: a leap second is a repeated or skipped second at the end of a UTC day,
/// and a device whose use for its time is printing it will not notice either. It is read because it
/// shares a byte with the version and the mode, and because "a leap second is pending tonight" is
/// worth having in the log on the day the log looks wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leap {
    /// No leap second is pending.
    Normal,

    /// The last minute of the day has 61 seconds: one is being added.
    Inserted,

    /// The last minute of the day has 59 seconds: one is being removed.
    Deleted,
}

impl fmt::Display for Leap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Normal => "no leap second pending",
            Self::Inserted => "a leap second is being added at the end of the day",
            Self::Deleted => "a leap second is being removed at the end of the day",
        })
    }
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
    // server that says it does not is not a source of time however plausible its packet looks.
    let leap = match first >> 6 {
        0 => Leap::Normal,
        1 => Leap::Inserted,
        2 => Leap::Deleted,
        _ => return Err(Refusal::Unsynchronized),
    };

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
        leap,
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
/// Published as a word by [`Self::to_word`] and read back by [`Self::from_word`], for the same
/// reason [`Link`] is: the task that asks a server and the task that keeps the clock are different
/// tasks, and what the first one learned has to reach the second one.
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

impl Obstruction {
    /// The word for "no attempt has failed", which is the state a chip boots in.
    ///
    /// Zero because it is not an obstruction to anything, and it is also what
    /// [`Self::from_word`] answers for a word no build of this firmware writes: before the first
    /// attempt, and something this build cannot describe, are both "nothing has gone wrong that this
    /// firmware has words for".
    pub const NONE: u32 = 0;

    /// The four bytes this is published as, lowest first.
    ///
    /// The tag is a number written down rather than derived from the order of the variants, for the
    /// same reason [`TAG_NO_NETWORK`] is: this is a wire format, and a match that can be read
    /// against the enum it encodes is worth more here than the few lines a derived number saves.
    /// Only [`Self::Refused`] puts anything in the second byte, and it is a [`Refusal`] whole rather
    /// than a name for one, because the refusal is eight sentences and each of them is the answer to
    /// a different thing a server can do wrong.
    const fn bytes(self) -> [u8; 4] {
        match self {
            Self::LookupTimedOut => [1, 0, 0, 0],
            Self::RequestTimedOut => [2, 0, 0, 0],
            Self::AnswerTimedOut => [3, 0, 0, 0],
            Self::NoServer => [4, 0, 0, 0],
            Self::Stranger => [5, 0, 0, 0],
            Self::WouldNotSend => [6, 0, 0, 0],
            Self::TooLong => [7, 0, 0, 0],
            Self::Refused(refusal) => [8, refusal_byte(refusal), 0, 0],
        }
    }

    /// The word this is published as, for [`Self::from_word`] to read back.
    ///
    /// `const` because the static in `src/clock.rs` that holds it has to be initialized by one.
    #[must_use]
    pub const fn to_word(self) -> u32 {
        u32::from_le_bytes(self.bytes())
    }

    /// The obstruction a published word stands for, or `None` for [`Self::NONE`] and for a word no
    /// build of this firmware writes.
    #[must_use]
    pub fn from_word(word: u32) -> Option<Self> {
        let [tag, refused, _, _] = word.to_le_bytes();

        match tag {
            1 => Some(Self::LookupTimedOut),
            2 => Some(Self::RequestTimedOut),
            3 => Some(Self::AnswerTimedOut),
            4 => Some(Self::NoServer),
            5 => Some(Self::Stranger),
            6 => Some(Self::WouldNotSend),
            7 => Some(Self::TooLong),
            8 => Some(Self::Refused(refusal_from_byte(refused))),
            _ => None,
        }
    }
}

/// The byte a refusal is published as.
const fn refusal_byte(refusal: Refusal) -> u8 {
    match refusal {
        Refusal::Short => 0,
        Refusal::NotAReply => 1,
        Refusal::Version => 2,
        Refusal::KissOfDeath => 3,
        Refusal::NotAServer => 4,
        Refusal::Unsynchronized => 5,
        Refusal::NotOurs => 6,
        Refusal::NoTime => 7,
        Refusal::BeforeTheEpoch => 8,
    }
}

/// The refusal a published byte stands for.
///
/// A byte no build of this firmware writes reads as [`Refusal::Short`], which is the one refusal that
/// is a fact about the packet rather than about its contents, and the one whose sentence describes
/// something that could not be read at all.
const fn refusal_from_byte(byte: u8) -> Refusal {
    match byte {
        0 => Refusal::Short,
        1 => Refusal::NotAReply,
        2 => Refusal::Version,
        3 => Refusal::KissOfDeath,
        4 => Refusal::NotAServer,
        5 => Refusal::Unsynchronized,
        6 => Refusal::NotOurs,
        7 => Refusal::NoTime,
        _ => Refusal::BeforeTheEpoch,
    }
}

/// How old an answer has to be before the time it set stops being called current.
///
/// One hour, and it is `src/ntp.rs`'s resync interval for the same reason it is chosen here: an
/// answer younger than that means the clock has been corrected since the last time the network was
/// checked, and an older one means the link went away and nothing since has said whether it came
/// back.
///
/// A time server's own answer is the whole of what this firmware knows about the accuracy of its
/// clock, so the honest reading of a clock nobody has confirmed for an hour is that it is drifting
/// on its own crystal — which is what this sentence says.
pub const STALE_AFTER_SECS: u64 = 60 * 60;

/// Where the time on the state line came from, and whether it can still be believed.
///
/// The two variants carry the reading they go with rather than leaving it to the caller, because the
/// pair is a single fact: a time that claims a server set it and renders as a count from boot is a
/// contradiction that nothing at the type level would otherwise catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Time {
    /// Nothing has set the clock, so it is counting from boot.
    SinceBoot {
        /// How long the chip has been running.
        elapsed_secs: u64,

        /// Why no server has answered, and `None` before the first attempt rather than an
        /// explanation of nothing having gone wrong.
        last: Option<Obstruction>,
    },

    /// A server has answered, and the clock has counted on since.
    FromServer {
        /// The time the server gave, plus everything this chip has counted since.
        epoch_secs: u64,

        /// How many steps the server was from a reference clock.
        stratum: u8,

        /// How long ago it answered, in seconds.
        age_secs: u64,
    },
}

impl Time {
    /// The clock counting from boot, with nothing to explain it yet.
    #[must_use]
    pub const fn since_boot(elapsed_secs: u64) -> Self {
        Self::SinceBoot {
            elapsed_secs,
            last: None,
        }
    }

    /// The clock counting from boot, with the last attempt that did not set it.
    #[must_use]
    pub const fn since_boot_after(elapsed_secs: u64, last: Obstruction) -> Self {
        Self::SinceBoot {
            elapsed_secs,
            last: Some(last),
        }
    }

    /// The clock a server has answered for, `age_secs` ago.
    #[must_use]
    pub const fn answered(epoch_secs: u64, stratum: u8, age_secs: u64) -> Self {
        Self::FromServer {
            epoch_secs,
            stratum,
            age_secs,
        }
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SinceBoot { elapsed_secs, last } => {
                write!(f, "{}", Clock::since_boot(*elapsed_secs))?;

                // The count and the reason, in one set of brackets: the count is what the greeting
                // has always printed, and the reason is what makes it readable as what it is.
                match last {
                    Some(obstruction) => write!(f, " (counting from boot: {obstruction})"),
                    None => f.write_str(" (counting from boot: nothing has answered yet)"),
                }
            }
            Self::FromServer {
                epoch_secs,
                stratum,
                age_secs,
            } => {
                write!(
                    f,
                    "{} UTC (from a stratum {stratum} server",
                    Clock::utc(*epoch_secs)
                )?;

                // Only once it is old enough to matter. This prints twice a second, so an age that
                // is always on the line is an age that stops being read the first time it is zero.
                if *age_secs >= STALE_AFTER_SECS {
                    write!(f, ", last confirmed {} ago", age(*age_secs))?;
                }

                f.write_str(")")
            }
        }
    }
}

/// How long something took, in as few words as say it: "45 seconds", "3 minutes", "1 hour 5 minutes".
///
/// The two largest units that are not zero, and no more than two, because this goes at the end of a
/// line that already has a date on it and a third unit is a precision nobody acts on — an hour is
/// either enough to go and look at the radio or it is not.
///
/// Public, and returning something that formats rather than a `String`, for one reason: this is a
/// decision — which units, how many, singular or plural — and a decision made in a private function
/// is a decision nothing checks. The choice of units is the same one the calendar in [`Clock`] makes
/// and is tested in the same place for the same reason. A `String` would have needed an allocator,
/// which this crate does not have and does not need for two numbers and two words.
#[must_use]
pub fn age(secs: u64) -> impl fmt::Display {
    Age(secs)
}

/// A number of seconds that knows how to write itself, in [`age`]'s units.
struct Age(u64);

impl fmt::Display for Age {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        /// The units of an age, largest first: the first two that are not zero are the two written.
        const UNITS: [(u64, &str); 4] = [
            (24 * 60 * 60, "day"),
            (60 * 60, "hour"),
            (60, "minute"),
            (1, "second"),
        ];

        let mut secs = self.0;
        let mut said = 0;

        for (size, name) in UNITS {
            let count = secs / size;

            // The remainder is carried into the next unit rather than dropped, because the units
            // below are the ones that share it: 1 hour 5 minutes is not 1 hour and 65 minutes.
            secs -= count * size;

            if count == 0 {
                continue;
            }

            if said == 2 {
                break;
            }

            if said > 0 {
                f.write_str(" ")?;
            }

            write!(f, "{count} {name}{}", plural(count))?;
            said += 1;
        }

        // Only an age of nothing reaches this, which nothing calls [`age`] with today. It is here
        // rather than left to write nothing, because a sentence with a hole in the middle of it is
        // worse than one that says zero.
        if said == 0 {
            return f.write_str("0 seconds");
        }

        Ok(())
    }
}

/// The `s` on the end of a unit of time, for the counts that need one.
const fn plural(count: u64) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Everything the firmware knows about itself, right now, as the line it prints.
///
/// The order is the decision as much as the wording is: the time first, because a reader who came to
/// see what time it is has their answer in the first field and can stop reading; then the address,
/// which is the next thing they want; and the state of the two mechanisms last, because that is what
/// they read when one of the first two is wrong, and by then they are looking for it.
///
/// A struct rather than three format arguments because these three come from three tasks, and three
/// arguments assembled in the greeting is three places for the log and the state to disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Where the time came from, and whether it can still be believed.
    pub time: Time,

    /// What the radio is doing, and why.
    pub link: Link,

    /// The address, or `None` while DHCP has not produced one.
    pub address: Option<Address>,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, ", self.time)?;

        match self.address {
            Some(address) => write!(f, "{address}, ")?,
            // Said rather than left out, because a missing clause reads as a formatting mistake and
            // the state of the radio is the answer to it.
            None => f.write_str("no address yet, ")?,
        }

        write!(f, "wifi: {}", self.link)
    }
}

/// How often the firmware reports what it is doing.
///
/// Five minutes, which is [`REPORT_EVERY_SECS`] rather than a number in the task that waits: this is
/// the one interval in the firmware that is a decision about a third party rather than about a
/// timing here — a server that is going to answer with an authentication error is not going to
/// answer differently because it was asked twice as often — and a decision with a third party in it
/// is one that belongs where something can read it.
pub const REPORT_EVERY_SECS: u64 = 5 * 60;

/// The kind of every event this firmware reports.
///
/// One value rather than a field, because there is one kind: what this chip is doing right now. A
/// second kind would be a second type here, and the difference between "the chip is up" and "the
/// chip was up" is a difference in when it is sent rather than in what the API stores about it.
pub const EVENT_TELEMETRY: &str = "telemetry";

/// The path the API takes an event on.
pub const EVENTS_PATH: &str = "/events";

/// One event, as the JSON the API stores.
///
/// The four fields are the API's, in its order, and the payload is a string rather than an object:
/// the API stores whatever JSON the body carries as text, so nesting a JSON document inside one is
/// this firmware's business and not the schema's — a server that wants to read it back is a
/// different API than one that wants to keep it.
///
/// A `Display` rather than a serializer because there is no serializer here and no allocator either:
/// this crate is `#![no_std]` with no dependencies, and the body is small enough that writing it
/// with [`fmt::Write`] is a few lines rather than a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event<'a> {
    /// What this chip is called, which is a name the API stores beside the event.
    pub device_id: &'a str,

    /// When the event happened, as seconds since the Unix epoch.
    ///
    /// A count and not a string because the firmware does not format a time here: it has one
    /// number and this is where it is handed over. What the number becomes is [`Timestamp`]'s
    /// business, and it is [`Timestamp`]'s business because that is the part that cannot be checked
    /// by looking at it.
    pub timestamp_secs: u64,

    /// What kind of event this is, which is [`EVENT_TELEMETRY`] for everything this firmware sends.
    pub event_type: &'a str,

    /// The state line, as [`crate::Status`] renders it.
    ///
    /// The whole sentence rather than the three fields behind it, so that what is stored is what the
    /// chip would have printed at that moment. Three fields in a payload would be a second way of
    /// writing the same line, and the two would drift.
    pub status: &'a Status,
}

impl fmt::Display for Event<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Written through a helper because a JSON string has one rule — a quote ends it — and four
        // places where getting it wrong produces something the server parses as a different document
        // rather than as an error. The status line is the only field here that could contain one: it
        // is a sentence this crate wrote, and a sentence that grows a quote is a bug worth a failing
        // test rather than a corrupt row in somebody's database.
        write!(
            f,
            "{{\"device_id\":{},\"timestamp\":\"{}\",\"event_type\":{},\"payload\":{}}}",
            Quoted(&self.device_id),
            Timestamp::at(self.timestamp_secs),
            Quoted(&self.event_type),
            Quoted(self.status),
        )
    }
}

/// A value inside a JSON string, quoted and with the characters that would end it escaped.
///
/// A `Display` rather than an `as_str`-and-paste so that escaping cannot be forgotten at one of the
/// call sites: the places above call this, and nothing else in this crate builds a JSON string by
/// hand. Over any `Display` rather than over `&str` because the payload is a [`Status`] — a sentence
/// this crate formats — and there is no allocator here to turn a formatted value into a `&str`.
///
/// `\` before `"` because JSON strings are what an escape is *for*, and a body with an unescaped
/// quote in it is a body the server rejects with a 400 rather than one it stores wrong. The control
/// characters are escaped as `\uXXXX` rather than left alone: JSON forbids them raw, and a status
/// line that grows one is a status line the API would refuse.
struct Quoted<'a>(&'a dyn fmt::Display);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The two quotes go straight to the formatter rather than through the escaping below, which
        // would escape them: they are the delimiters, and the thing being quoted is what goes
        // between them.
        //
        // Written this way rather than into a `String` that is then quoted, because this crate has no
        // allocator: escaping has to happen as the characters are produced.
        f.write_char('"')?;

        {
            let mut escaping = Escaping { inner: f };

            write!(escaping, "{}", self.0)?;
        }

        f.write_char('"')
    }
}

/// A [`fmt::Write`] that escapes what is written to it before it reaches the writer underneath.
///
/// There rather than as a `String` because the value being escaped is formatted, not borrowed: a
/// [`Status`] becomes its sentence through `fmt`, and the only place the sentence exists before the
/// bytes go out is here.
struct Escaping<'a, 'b> {
    /// Where the escaped characters go.
    inner: &'a mut fmt::Formatter<'b>,
}

impl fmt::Write for Escaping<'_, '_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for character in text.chars() {
            match character {
                '"' => self.inner.write_str("\\\"")?,
                '\\' => self.inner.write_str("\\\\")?,
                '\n' => self.inner.write_str("\\n")?,
                '\r' => self.inner.write_str("\\r")?,
                '\t' => self.inner.write_str("\\t")?,
                // The rest of the control characters, as JSON spells them: `\u0000` and a name for
                // each of the twenty or so that are not printable. JSON has no name for a null or a
                // bell, so the number is the only spelling available.
                control if control < ' ' => write!(self.inner, "\\u{:04x}", u32::from(control))?,
                other => self.inner.write_char(other)?,
            }
        }

        Ok(())
    }
}

/// An instant, in the form RFC 3339 writes one in UTC.
///
/// `2026-10-05T20:41:59Z`, and it is a type rather than a call to [`fmt::Write`] in the middle of
/// [`Event`]'s because the format is the decision and the format is what a host can check: an API
/// that stores the string and a reader of the state line that prints `2026-10-05 20:41:59` want two
/// different renderings of one number, and which is which is not obvious from either.
///
/// UTC with a `Z` and no offset, for the same reason [`Clock::Utc`] is UTC: this is what an API
/// expects, and a rendering with an offset in it would be a rendering this firmware has no way to
/// produce — there is no timezone on this chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp(u64);

impl Timestamp {
    /// The instant that many seconds after 1970-01-01T00:00:00Z.
    #[must_use]
    pub const fn at(epoch_secs: u64) -> Self {
        Self(epoch_secs)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let civil = civil(self.0);

        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            civil.year, civil.month, civil.day, civil.hour, civil.minute, civil.second
        )
    }
}

/// What a status line from the API means.
///
/// The body is not read. It is `{"ok":true}` on a store and `{"error":"Unauthorized"}` on a refusal,
/// and the status line already says which of the two it is — parsing a second JSON document on a
/// microcontroller to learn what three digits said for free is a way to have a second thing that can
/// be wrong about the API.
///
/// A sentence per case rather than the number, because the number is what the API said and the
/// sentence is what to do about it: 401 here means one specific thing, which is that the request
/// went out with no credentials on it, and it is going to keep meaning that until a token is added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 201: the event was stored.
    Stored,

    /// 401: the API refused the event because the request carried no credentials it accepts.
    ///
    /// What this firmware gets today, on purpose. It sends no `Authorization` header at all, so
    /// this is the answer that says the plumbing works and the authentication is missing, which is
    /// two facts in one status line.
    Unauthorized,

    /// 400: the API could not read the event, which is a body this firmware built wrongly.
    Malformed,

    /// 405: the path was reached by a method the API does not take, which would be a bug here.
    NotAllowed,

    /// Any other status: reported as the number rather than guessed at.
    ///
    /// A named case for the ones that mean something specific to this exchange and `Unexpected` for
    /// the rest, because an API that grows a status code should land somewhere a reader recognizes
    /// as "the API said no, and here is which no" rather than in a sentence written for a case it
    /// is not.
    Unexpected(u16),

    /// The answer did not begin with something that could be a status line.
    ///
    /// Its own case rather than folded into `Unexpected`, because it is a different problem: the
    /// other means the API answered and this means whatever answered was not the API. A captive
    /// portal is the usual one, and a board on a hotel network is exactly where that happens.
    NotAnAnswer,
}

impl Verdict {
    /// What a status line says, or [`Self::NotAnAnswer`] for something that is not one.
    ///
    /// Total rather than an `Option`: the caller prints this either way, and a value it has to
    /// handle before it can say anything is a place for the handling to be forgotten.
    #[must_use]
    pub fn from_status_line(line: &[u8]) -> Self {
        let Some(status) = status_code(line) else {
            return Self::NotAnAnswer;
        };

        match status {
            201 => Self::Stored,
            400 => Self::Malformed,
            401 => Self::Unauthorized,
            405 => Self::NotAllowed,
            other => Self::Unexpected(other),
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stored => f.write_str("the API stored the event"),
            Self::Unauthorized => {
                f.write_str("the API refused the event: no credentials were sent with it")
            }
            Self::Malformed => {
                f.write_str("the API could not read the event: it rejected the body")
            }
            Self::NotAllowed => f.write_str("the API would not take a POST on this path"),
            Self::Unexpected(status) => {
                write!(
                    f,
                    "the API answered with a status this firmware does not name: {status}"
                )
            }
            Self::NotAnAnswer => {
                f.write_str("what answered was not the API: the first line was not a status line")
            }
        }
    }
}

/// The three digits of a status code, from the first line of an answer.
///
/// Reads the code rather than the reason phrase, because the phrase is prose that a server may
/// change and the code is the part of the line that means something. The line is not required to
/// end anywhere in particular: a read that arrived in pieces is a line this still has to make sense
/// of, so the search is for the first space and then three digits, and the rest is ignored.
///
/// `None` for anything that is not `HTTP/x.y NNN`, which is what makes [`Verdict::NotAnAnswer`]
/// reachable at all — a portal's login page is HTML, and the first fifteen bytes of it are a
/// `<!DOCTYPE` that is not a version number.
fn status_code(line: &[u8]) -> Option<u16> {
    let after_version = line.strip_prefix(b"HTTP/")?;
    // The version and its single digit, then the space. `strip_prefix` and a search for the space
    // rather than `split_once`, which is still unstable for slices on the pinned toolchain — and a
    // hand-rolled one would be the third spelling of the same two operations in this crate.
    let space = after_version.iter().position(u8::is_ascii_whitespace)?;
    let after_space = after_version.get(space + 1..)?;
    let digits = after_space.get(..3)?;

    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }

    // A space or the end of the buffer after the code, so that `HTTP/1.1 2011 Created` is not read
    // as 201: a four-digit field is not a status code, and a parser that takes the first three
    // digits of it is reading a length or a version that happens to be next door.
    match after_space.get(3) {
        None | Some(b' ') => {}
        Some(_) => return None,
    }

    // The digits as text rather than accumulated as a number, so that a line saying `HTTP/1.1 20`
    // or `HTTP/1.1 2011` is refused instead of read as 20 or 201: the code is three digits, and a
    // parser that takes what it finds would make a length or a version out of the field next to it.
    let digits = core::str::from_utf8(digits).ok()?;

    digits.parse().ok()
}

/// The bytes of one HTTP request, head and body, as they go on the wire.
///
/// A `Display` rather than something that writes into a buffer this crate owns, because this crate
/// does not know how big the request is: the body carries a [`Status`], whose length depends on the
/// address DHCP handed out and on how long the sentence about the radio is. The firmware writes it
/// into a buffer it sized, and if the buffer is too small that is a fact about the buffer rather than
/// about the request.
///
/// `content_length` is a parameter rather than something derived, for the same reason: it is the
/// number of bytes of body that were written, and the caller is the only thing that knows it. The
/// head cannot be built without it, which is why the body goes first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request<'a> {
    /// The host to send to, which is what the `Host` header carries.
    ///
    /// The name and not the address: `embassy-net` resolves it, and an HTTP client that sent the
    /// resolved address in this header would produce a request a server may refuse and a log that
    /// cannot be read against the name somebody typed.
    pub host: &'a str,

    /// The path, which is [`EVENTS_PATH`] for this exchange.
    pub path: &'a str,

    /// How many bytes of body follow the head.
    pub content_length: usize,
}

impl fmt::Display for Request<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // CRLF and not `\n`, because this is HTTP/1.1 and not a text file: a server is entitled to
        // refuse a request whose lines end in a bare newline, and "entitled to" is the whole reason
        // this is written down rather than left to whatever the format string felt like.
        //
        // `Content-Length` is here rather than a `Transfer-Encoding` because the body was written
        // into a buffer before any of this was: its length is known without counting the bytes as
        // they go past, which is what a chunked request would need.
        //
        // `Connection: close` because this client reads the answer and does nothing else with the
        // connection. Without it the server may hold the socket open and the read above waits for
        // bytes that are not coming, which is a timeout on every single report rather than once.
        write!(
            f,
            "POST {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            self.path, self.host, self.content_length,
        )
    }
}
