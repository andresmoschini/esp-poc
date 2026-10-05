//! What the firmware decides for itself, decided here so that something can check it.
//!
//! Six decisions live in this crate, and every one of them is one the firmware would otherwise get
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

use core::fmt;
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
