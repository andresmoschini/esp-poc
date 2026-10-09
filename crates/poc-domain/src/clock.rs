//! What the firmware believes the time is, how it counts on, and how either is written.
//!
//! Three renderings of one number live here, and they are three because three things read it: the
//! greeting prints a time of day or a date, the reported event needs something a server can sort,
//! and a line that says how long ago something happened wants a count of seconds. Sharing one
//! calendar between them is what keeps them from drifting.
//!
//! [`Time`] is the shape the firmware publishes — a reading plus where it came from — and
//! [`Clock`] is what the state line renders it as. [`STALE_AFTER_SECS`] is how old an answer has to
//! be before the time it set stops being called current.

use core::fmt;

use crate::ntp::Obstruction;

/// Seconds in a day, which is as far as [`Clock::SinceBoot`] goes before it starts again.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// What the firmware believes the time is, and where that belief comes from.
///
/// The two variants are different in kind and the difference is the point of the type. There is no
/// battery-backed clock on this chip, so [`Self::SinceBoot`] is the only reading it can make on its
/// own: the scheduler's counter, which starts at zero when the firmware starts. A real time has to
/// come from the network, and until one has arrived the two are told apart by their shape rather
/// than by anything else, which is why [`Self::Utc`] prints a date and time and this one a
/// time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    /// How long the chip has been running, wrapped into a day.
    ///
    /// This is not a time of day and is not meant to read like one. It is written as `HH:MM:SS`
    /// because that is the shape a reader of a serial log is looking for, and it wraps at midnight
    /// rather than counting hours forever so that it cannot be mistaken for a clock that stopped.
    SinceBoot(u64),

    /// A time in UTC, as a count of seconds since the Unix epoch.
    ///
    /// UTC and nothing else: this is what a network time source hands out, and a reading that
    /// disagrees with it by a timezone has been adjusted by somebody, which is not this crate's
    /// business. The count is unsigned because a time before 1970 is not a time this can print,
    /// and an SNTP server that reports one is refused rather than rendered — see [`crate::ntp::sntp_reply`].
    /// Printed as RFC 3339, the same rendering as [`Timestamp`]: the greeting that carries it is
    /// read twice a second, and one shared shape is one less thing for a reader to learn.
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
            // RFC 3339, shared with [`Timestamp`]: one calendar rendering for the greeting and
            // the reported event, so a line on the serial log reads the same as its row.
            Self::Utc(epoch_secs) => write!(f, "{}", Timestamp::at(*epoch_secs)),
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
/// common cases and wrong in February, and `tests/report_api.rs` pins the ends of each of them
/// through [`Timestamp`], which is also the rendering [`Clock::Utc`] shares.
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
                    "{} (from a stratum {stratum} server",
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

/// How long something took, in seconds.
///
/// A bare count rather than words: "3661s" instead of "1 hour 1 minute". Two units with
/// singulars and plurals is a precision nobody acts on at the end of a line that already has a
/// date on it, and a count is nothing to test beyond the number.
///
/// Public, and returning something that formats rather than a `String`, for one reason: this is a
/// decision — seconds, always — and a decision made in a private function is a decision nothing
/// checks. A `String` would have needed an allocator, which this crate does not have and does not
/// need for one number and one letter.
#[must_use]
pub fn age(secs: u64) -> impl fmt::Display {
    Age(secs)
}

/// A number of seconds that knows how to write itself, in [`age`]'s units.
struct Age(u64);

impl fmt::Display for Age {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", self.0)
    }
}

/// An instant, in the form RFC 3339 writes one in UTC.
///
/// `2026-10-05T20:41:59Z`, and it is a type rather than a call to [`fmt::Write`] in the middle of
/// [`crate::event::Event`]'s because the format is the decision and the format is what a host can check: an API
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
