//! The chip's clock: what time it is, and where that answer came from.
//!
//! There is no battery-backed clock on this chip, so a time that survives a power cycle is not
//! something this firmware can have: what lives here is in RAM, and it is asked for again over the
//! network every time the chip boots. Between two answers the scheduler's monotonic counter counts
//! on, so the clock keeps moving without asking — which also means it drifts, by however much the
//! crystal drifts, until the next answer puts it right.
//!
//! That is the whole of the device's timekeeping: one number, set from outside. `src/ntp.rs` is
//! what sets it, and `src/status.rs` is what prints it, and neither of them knows how the number
//! got there.
//!
//! ## What the clock can say about itself
//!
//! A time on its own is not enough to read: `18:22:31` from a count since boot and `18:22:31` from a
//! server are the same six characters and mean different things. So the clock keeps three facts
//! about where it is — whether a server has answered, how many steps that server was from a
//! reference clock, and how long ago it answered — plus the last thing that went wrong asking, which
//! is the answer to "why is this still counting from boot". [`time`] puts the four of them together
//! into a [`poc_report::Time`], and the wording and the arithmetic of that are in `poc-report` where
//! a host can check them.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, Ordering};

use embassy_time::Instant;
use poc_report::{Obstruction, Time};

/// Whether a time source has answered yet.
static ANSWERED: AtomicBool = AtomicBool::new(false);

/// The number of seconds between this chip's own count of seconds and real time, at the moment of
/// the last answer.
///
/// The offset rather than the time itself, and the difference is small where a date is not: what
/// [`time`] has to do with an answer is add the time that has passed since to it, so the difference
/// between a real time and this chip's zero is all the arithmetic ever needs. A count of seconds
/// since 1970 is not a small difference, and an `i32` of them is [`storable`]'s whole job.
static OFFSET: AtomicI32 = AtomicI32::new(0);

/// How many steps from a reference clock the server that answered last was.
///
/// A number rather than a sentence, because there is nothing useful to compare it against and a
/// firmware that invented a threshold would be guessing. What it is good for is noticing that it
/// changed, and for saying where the time on the greeting came from.
static STRATUM: AtomicU8 = AtomicU8::new(0);

/// How long the chip had been running when a server last answered, in seconds.
///
/// An `u32` rather than a `u64` because 136 years of running time is more than the clock will ever
/// hold, and the saturation in [`seconds_in_a_word`] is what it reaches rather than something a
/// reader of the greeting can.
static ANSWERED_AT: AtomicU32 = AtomicU32::new(0);

/// What stood between this chip and a time, the last time something did.
///
/// Kept here rather than in `src/ntp.rs` because the question it answers — why is the time still
/// counting from boot — is about the clock, and because [`time`] reads it in the same breath as the
/// rest. A word rather than the whole type, for the same reason and because [`Obstruction`] is
/// published for exactly this: one value, written once per failed attempt, read until the next one.
static OBSTRUCTED: AtomicU32 = AtomicU32::new(Obstruction::NONE);

/// Where the clock was last set, and how long ago.
///
/// Two facts that only mean anything together — a stratum from an hour ago is not a fact about the
/// time being printed — so they are returned as one rather than as two getters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    /// How many steps the server that answered was from a reference clock: one is a clock that is
    /// itself a reference, such as an atomic clock or a GPS receiver.
    pub stratum: u8,

    /// How long ago it answered, in seconds.
    pub age_secs: u64,
}

/// Sets the clock from a count of seconds since the Unix epoch, and from the server that gave it.
///
/// A later answer overwrites an earlier one, which is what being handed better information earns:
/// nothing here knows whether the new time is earlier than the old, and deciding that would mean
/// trusting a number from the network over one already in RAM.
///
/// The stratum is part of the call rather than a second call because an answer published without it
/// is an answer published half of: the greeting says where the time came from, and "from somewhere"
/// is not an answer.
pub fn set(epoch_secs: u64, stratum: u8) {
    let running = uptime_secs();

    // Stored in this order, and read as if it arrived in this order. A reader that saw the flag
    // without the words behind it would have no idea whether there was anything to read, and one
    // that saw a stale one would be off by however much the clock had moved between two answers.
    // The release store and the acquire loads are what make the set atomic from the reader's side;
    // the chip has one core, so this is the only ordering that matters here.
    OFFSET.store(storable(epoch_secs, running), Ordering::Release);
    STRATUM.store(stratum, Ordering::Relaxed);
    ANSWERED_AT.store(seconds_in_a_word(running), Ordering::Relaxed);

    // An answer clears the last failure: the reason there was no time was that the last attempt did
    // not produce one, and there is one now.
    OBSTRUCTED.store(Obstruction::NONE, Ordering::Relaxed);

    ANSWERED.store(true, Ordering::Release);
}

/// Reports that an attempt to ask a time server did not produce one.
///
/// The reason is kept rather than only printed, because "why is this still counting from boot" is
/// the question the greeting raises twice a second and the log answered only once.
pub fn report_failure(obstruction: Obstruction) {
    OBSTRUCTED.store(obstruction.to_word(), Ordering::Release);
}

/// What the firmware believes the time is, and why it believes it.
///
/// This never waits and never fails: it is a handful of atomic loads and a subtraction, which is what
/// lets the greeting call it twice a second without knowing anything about the network. What it
/// cannot say is anything the four facts it reads do not add up to, which is the point — the shape of
/// the answer says whether it is a time of day or a count from boot, and now also whether a server
/// confirmed it recently enough to be believed.
#[must_use]
pub fn time() -> Time {
    if let Some(source) = source() {
        return Time::answered(epoch_secs(), source.stratum, source.age_secs);
    }

    let elapsed_secs = uptime_secs();

    // Nothing to explain yet: a chip that has not asked a server anything has not had a failure, and
    // the word for that is the same one the static starts as. Saying what the last attempt came to
    // before there has been one is the kind of guess the rest of this file is careful not to make.
    match last_obstruction() {
        Some(obstruction) => Time::since_boot_after(elapsed_secs, obstruction),
        None => Time::since_boot(elapsed_secs),
    }
}

/// What the last answer was, and how long ago it came, or `None` if there has not been one.
fn source() -> Option<Source> {
    if !ANSWERED.load(Ordering::Acquire) {
        return None;
    }

    let answered_at = u64::from(ANSWERED_AT.load(Ordering::Acquire));

    Some(Source {
        stratum: STRATUM.load(Ordering::Acquire),
        age_secs: uptime_secs().saturating_sub(answered_at),
    })
}

/// What stood between this chip and a time, the last time something did.
fn last_obstruction() -> Option<Obstruction> {
    Obstruction::from_word(OBSTRUCTED.load(Ordering::Acquire))
}

/// Seconds since the Unix epoch, as this chip's own count plus the offset of the last answer.
fn epoch_secs() -> u64 {
    let epoch_secs = i64::from(OFFSET.load(Ordering::Acquire))
        .saturating_add(i64::try_from(uptime_secs()).unwrap_or(i64::MAX));

    u64::try_from(epoch_secs).unwrap_or(u64::MAX)
}

/// How long the chip has been running.
///
/// What the age of an answer is measured against, and what the offset is applied to on the way to a
/// date. The count is the scheduler's own, which starts at zero when the firmware does.
fn uptime_secs() -> u64 {
    Instant::now().as_secs()
}

/// A count of seconds as one 32-bit atomic can hold it, saturating rather than wrapping.
///
/// 136 years of running time, which is not a thing that happens to a proof of concept: it is here so
/// that the value a reader sees is a large one rather than a small one that looks real.
fn seconds_in_a_word(secs: u64) -> u32 {
    u32::try_from(secs).unwrap_or(u32::MAX)
}

/// The offset between a real time and this chip's zero, in the one word a reader can take whole.
///
/// A count of seconds fits in about 68 years either way of the chip's own zero, and a time further
/// from that than 68 years is not one this firmware can put anywhere near a wall. Saturating rather
/// than wrapping is the point: a wrapped offset would be a date in the past that looks like a real
/// one, and a saturated one is 68 years out, which does not.
///
/// The date this stops serving correctly on is 2038-01-19, which is where an `i32` of seconds since
/// 1970 runs out — a reading after it lags by however far past that it is, rather than jumping
/// backwards. Fixing it means a 64-bit static, which no reader on this target could take in one go.
fn storable(epoch_secs: u64, since_boot_secs: u64) -> i32 {
    let since_boot = i64::try_from(since_boot_secs).unwrap_or(i64::MAX);
    let offset = i64::try_from(epoch_secs)
        .unwrap_or(i64::MAX)
        .saturating_sub(since_boot);

    i32::try_from(offset).unwrap_or(if offset < 0 { i32::MIN } else { i32::MAX })
}
