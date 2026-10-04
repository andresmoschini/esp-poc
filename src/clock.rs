//! The chip's clock: what time it is, and where that answer came from.
//!
//! There is no battery-backed clock on this chip, so a time that survives a power cycle is not
//! something this firmware can have: what lives here is in RAM, and it is asked for again over the
//! network every time the chip boots. Between two answers the scheduler's monotonic counter counts
//! on, so the clock keeps moving without asking — which also means it drifts, by however much the
//! crystal drifts, until the next answer puts it right.
//!
//! That is the whole of the device's timekeeping: one number, set from outside. `src/ntp.rs` is
//! what sets it, and `src/bin/main.rs` is what prints it, and neither of them knows how the number
//! got there.

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use embassy_time::Instant;
use poc_report::Clock;

/// Whether a time source has answered yet.
static ANSWERED: AtomicBool = AtomicBool::new(false);

/// The number of seconds between this chip's own count of seconds and real time, at the moment of
/// the last answer.
///
/// The offset rather than the time itself, for two reasons that turn out to be the same reason.
/// What [`now`] has to do with an answer is add the time that has passed since to it, so the
/// difference between a real time and this chip's zero is all the arithmetic ever needs — and that
/// difference is small, where a date is not. It also has to be small: this core has 32-bit atomics
/// and no 64-bit ones, so a count of seconds since 1970 cannot be one atomic no matter what it is
/// asked to hold.
static OFFSET: AtomicI32 = AtomicI32::new(0);

/// Sets the clock from a count of seconds since the Unix epoch.
///
/// A later answer overwrites an earlier one, which is what being handed better information earns:
/// nothing here knows whether the new time is earlier than the old, and deciding that would mean
/// trusting a number from the network over one already in RAM.
pub fn set(epoch_secs: u64) {
    // Stored in this order, and read as if it arrived in this order. A reader that saw the flag
    // without the offset would have no idea whether there was an offset to read, and a reader that
    // saw a stale one would be off by however much the clock had moved between the two answers.
    // The release store and the acquire loads are what make the pair atomic from the reader's side;
    // the chip has one core, so this is the only ordering that matters here.
    OFFSET.store(storable(epoch_secs), Ordering::Release);
    ANSWERED.store(true, Ordering::Release);
}

/// What time it is, and whether that is a real time or the chip counting from boot.
///
/// This never waits and never fails: it is two atomic loads and a subtraction, which is what lets
/// the greeting call it twice a second without knowing anything about the network. The shape of the
/// reading says which of the two it is — a date means an answer arrived, a bare time of day means it
/// has not — and the firmware prints that difference rather than presenting both as the same thing.
#[must_use]
pub fn now() -> Clock {
    let since_boot = i64::try_from(Instant::now().as_secs()).unwrap_or(i64::MAX);

    if !ANSWERED.load(Ordering::Acquire) {
        return Clock::since_boot(u64::try_from(since_boot).unwrap_or(u64::MAX));
    }

    let epoch_secs = i64::from(OFFSET.load(Ordering::Acquire)).saturating_add(since_boot);

    Clock::utc(u64::try_from(epoch_secs).unwrap_or(u64::MAX))
}

/// The offset between a real time and this chip's zero, in as many bits as this core can hold.
///
/// A count of seconds fits in about 68 years either way of the chip's own zero, and a time further
/// from that than 68 years is not one this firmware can put anywhere near a wall. Saturating rather
/// than wrapping is the point: a wrapped offset would be a date in the past that looks like a real
/// one, and a saturated one is 68 years out, which does not.
fn storable(epoch_secs: u64) -> i32 {
    let since_boot = i64::try_from(Instant::now().as_secs()).unwrap_or(i64::MAX);
    let offset = i64::try_from(epoch_secs)
        .unwrap_or(i64::MAX)
        .saturating_sub(since_boot);

    i32::try_from(offset).unwrap_or(if offset < 0 { i32::MIN } else { i32::MAX })
}
