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
//! server are the same six characters and mean different things. So the clock keeps the last answer
//! and how long ago it came, plus the last thing that went wrong asking, which is the answer to "why
//! is this still counting from boot". [`time`] puts the four of them together into a
//! [`poc_report::Time`], and the wording and the arithmetic of that are in `poc-report` where a host
//! can check them.
//!
//! ## Why one lock and not five atomics
//!
//! Everything here is four small facts that only mean anything together — an answer's time, its
//! stratum and how long ago it came, or the failure that says there is no answer — and they are
//! written by the task that talks to the network and read by the greeting, which runs twice a second
//! on a chip with one core. A lock around the whole of it is a brief critical section taken twice a
//! second, which is the same argument `src/wifi.rs` already makes for the radio's `Link`, and it is
//! the right one here for a reason the atomics could not give: five one-word statics holding five
//! parts of one fact need an ordering invariant to say they belong together, and an invariant nothing
//! can check is an invariant that eventually is not upheld.
//!
//! Nor is there any word width to trade against. Neither chip has a RISC-V atomic wider than a word,
//! so the 64-bit timestamp this needs would have been published through `portable-atomic` and read
//! one chunk at a time — which is a second thing to get right, for no gain over a lock that is
//! already there.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use embassy_time::Instant;
use poc_report::{Obstruction, Time};

/// What the clock knows: the last answer it was given, and the last thing that stopped one arriving.
///
/// Published as the values themselves rather than as one word encoding of them, for the reason
/// `src/wifi.rs` publishes its `Link` that way: a value split across several words is a value whose
/// parts can be read at different moments. The `RefCell` is what makes the write safe without an
/// `unsafe` — nothing locks again inside a lock closure and no interrupt handler touches this
/// static, so the borrow cannot fail, and if that ever stops being true it panics rather than
/// corrupting.
///
/// It starts with nothing because that is the state this chip is in from the moment it starts: no
/// server has answered and no attempt has failed.
static FACTS: Mutex<CriticalSectionRawMutex, RefCell<Facts>> = Mutex::new(RefCell::new(Facts {
    answer: None,
    obstruction: None,
}));

/// One answer from a time server, as it was when it arrived.
///
/// The time the server gave rather than an offset from this chip's own zero, which is what used to be
/// stored: the difference between the two is all the arithmetic ever needed, and an offset has to be
/// squeezed into a type narrow enough to be atomic — where the count of seconds since 1970 is not,
/// and where the day the firmware would have stopped serving a correct date is decided by the width
/// of a word rather than by anything about the chip or the network.
#[derive(Debug, Clone, Copy)]
struct Answer {
    /// Seconds since the Unix epoch, which is 1970-01-01T00:00:00Z.
    epoch_secs: u64,

    /// How many steps the server was from a reference clock.
    stratum: u8,

    /// How long the chip had been running when the answer arrived.
    answered_at_secs: u64,
}

/// Everything the clock holds, and the whole of what it knows.
#[derive(Debug, Clone, Copy)]
struct Facts {
    /// The last answer, or `None` before the first one.
    ///
    /// `None` rather than a zero timestamp, because a zero is a time — 1970-01-01T00:00:00Z — and
    /// this firmware knows how to print one.
    answer: Option<Answer>,

    /// What stood between this chip and a time, the last time something did, and `None` before the
    /// first attempt rather than an explanation of nothing having gone wrong.
    obstruction: Option<Obstruction>,
}

/// Where the clock was last set, and how far it has counted on since.
///
/// Three facts that only mean anything together — a stratum from an hour ago is not a fact about the
/// time being printed — so they are returned as one rather than as three getters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Source {
    /// The time the server that answered gave, plus everything this chip has counted since.
    pub epoch_secs: u64,

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
    FACTS.lock(|slot| {
        let mut facts = slot.borrow_mut();

        // The running time is read once, here, rather than again on the way out: the age of this
        // answer is measured against the moment it arrived, and a reader that measured it against a
        // later one would be a reader reporting the clock as older than it is.
        facts.answer = Some(Answer {
            epoch_secs,
            stratum,
            answered_at_secs: uptime_secs(),
        });

        // An answer clears the last failure: the reason there was no time was that the last attempt
        // did not produce one, and there is one now.
        facts.obstruction = None;
    });
}

/// Reports that an attempt to ask a time server did not produce one.
///
/// The reason is kept rather than only printed, because "why is this still counting from boot" is
/// the question the greeting raises twice a second and the log answered only once.
pub fn report_failure(obstruction: Obstruction) {
    FACTS.lock(|slot| slot.borrow_mut().obstruction = Some(obstruction));
}

/// What the firmware believes the time is, and why it believes it.
///
/// This never waits and never fails: it is one brief critical section and a subtraction, which is
/// what lets the greeting call it twice a second without knowing anything about the network. What
/// it cannot say is anything the facts it reads do not add up to, which is the point — the shape of
/// the answer says whether it is a time of day or a count from boot, and now also whether a server
/// confirmed it recently enough to be believed.
#[must_use]
pub fn time() -> Time {
    let elapsed_secs = uptime_secs();

    FACTS.lock(|slot| {
        let facts = slot.borrow();

        match source(&facts, elapsed_secs) {
            Some(source) => Time::answered(source.epoch_secs, source.stratum, source.age_secs),

            // Nothing to explain yet: a chip that has not asked a server anything has not had a
            // failure, and the word for that is the same one the state starts as. Saying what the
            // last attempt came to before there has been one is the kind of guess the rest of this
            // file is careful not to make.
            None => match facts.obstruction {
                Some(obstruction) => Time::since_boot_after(elapsed_secs, obstruction),
                None => Time::since_boot(elapsed_secs),
            },
        }
    })
}

/// What the last answer was, and what the clock has counted on to since, or `None` if there has not
/// been one.
///
/// The arithmetic is addition rather than an offset reapplied: the time the server gave plus however
/// long the chip has been running since it answered is the same number, and it is the one that can
/// be held in a word this chip actually has.
fn source(facts: &Facts, uptime_secs: u64) -> Option<Source> {
    let answer = facts.answer?;

    // Saturating rather than wrapping: a wrapped age would be a small number that looks like a real
    // one, and the only way to reach it is a counter that has gone backwards.
    let age_secs = uptime_secs.saturating_sub(answer.answered_at_secs);

    Some(Source {
        epoch_secs: answer.epoch_secs.saturating_add(age_secs),
        stratum: answer.stratum,
        age_secs,
    })
}

/// How long the chip has been running.
///
/// What the age of an answer is measured against, and what an answer is added to on the way to a
/// date. The count is the scheduler's own, which starts at zero when the firmware does.
fn uptime_secs() -> u64 {
    Instant::now().as_secs()
}
