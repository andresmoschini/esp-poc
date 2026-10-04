// The tests for the sentences the firmware prints.
//
// They live in `tests/` rather than in a `#[cfg(test)]` module because this crate is `#![no_std]`:
// an integration test is a separate crate with the standard prelude, so `assert_eq!` and `String` are
// there without the library giving up `no_std` for its own build. They run on the machine running the
// gate, which is the whole reason this crate exists — the rest of the firmware cannot be compiled for
// a host at all, and cannot be run anywhere CI has.

use std::fmt::Write as _;

use poc_report::{Address, Clock, JoinFailure, Reason};

/// Seconds in a day, restated from the library because the wrapping is the property under test and
/// naming it here is what keeps the numbers below readable.
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Renders a value the way the firmware's `Display2Format` does on the chip.
///
/// `core::fmt` is the same code in both places, so this is not an approximation of what the board
/// prints: it is the same formatting, reached without a logger.
fn render(value: &impl std::fmt::Display) -> String {
    let mut written = String::new();

    write!(written, "{value}").expect("writing to a String cannot fail");

    written
}

/// An address is written the way a router writes one.
///
/// The prefix is what makes this worth a type at all: the greeting in `src/bin/main.rs` and the report
/// in `src/wifi.rs` both print an address, and printing the prefix in one of them and not the other is
/// exactly the drift this crate was made to stop.
#[test]
fn an_address_carries_its_prefix() {
    let address = Address {
        ip: "192.168.0.225".parse().unwrap(),
        prefix_len: 24,
    };

    assert_eq!(render(&address), "192.168.0.225/24");
}

/// A prefix of zero is a route rather than a network, and a prefix of 32 is a bare address; both have
/// to survive the formatting rather than being treated as missing.
#[test]
fn both_ends_of_the_prefix_range_are_printed() {
    let host = "10.0.0.7".parse().unwrap();

    assert_eq!(
        render(&Address {
            ip: host,
            prefix_len: 0
        }),
        "10.0.0.7/0",
    );
    assert_eq!(
        render(&Address {
            ip: host,
            prefix_len: 32
        }),
        "10.0.0.7/32",
    );
}

/// Every reason says something a person can act on, and says the same thing wherever it is printed.
///
/// This is a table rather than one test per reason because the property is that the set is covered: a
/// variant added to the enum without a sentence here fails to compile, which is the point. The
/// sentences are also the firmware's whole contribution to a serial log full of driver output, so they
/// are pinned verbatim — a rewording that reads better is a deliberate edit here, not an accident.
#[test]
fn every_reason_has_a_sentence() {
    let expected = [
        (Reason::NoSuchNetwork, "nothing with that name was heard"),
        (
            Reason::SecurityRefused,
            "the network refused these credentials",
        ),
        (
            Reason::NoAnswer,
            "the network stopped answering partway through",
        ),
        (Reason::LinkLost, "the link came up and then went down"),
        (
            Reason::Other,
            "the radio reported a reason this firmware does not name",
        ),
    ];

    for (reason, sentence) in expected {
        assert_eq!(render(&reason), sentence, "sentence for {reason:?}");
    }
}

/// The four named reasons are four different problems, and two failures that read alike are the two
/// that most often get confused: a wrong password and a station that is too far away both arrive as an
/// exchange that stops. If these ever collapse into one sentence, the distinction that matters on the
/// board is gone.
#[test]
fn the_reasons_are_distinguishable() {
    let sentences = [
        Reason::NoSuchNetwork,
        Reason::SecurityRefused,
        Reason::NoAnswer,
        Reason::LinkLost,
    ]
    .map(|reason| render(&reason));

    let mut unique = sentences.to_vec();
    unique.sort_unstable();
    unique.dedup();

    assert_eq!(unique.len(), sentences.len(), "two reasons read the same");
}

/// The signal is printed when there is one, because it is what separates a refused password from a
/// station that cannot be heard.
#[test]
fn a_failure_prints_the_signal_it_has() {
    let failure = JoinFailure {
        reason: Reason::NoAnswer,
        signal: Some(-81),
    };

    assert_eq!(
        render(&failure),
        "the network stopped answering partway through (signal -81 dBm)",
    );
}

/// A weak reading is still a reading, and is the one that matters most: -81 dBm is what this looks like
/// when the access point is too far away, and a firmware that hid the sign would hide the diagnosis.
#[test]
fn a_weak_signal_is_not_hidden() {
    let failure = JoinFailure {
        reason: Reason::NoAnswer,
        signal: Some(-81),
    };

    assert!(render(&failure).contains("-81 dBm"), "{}", render(&failure));
}

/// The radio fills in -128 when it has no reading, which is what the firmware passes as `None`. There
/// is nothing to add in that case, and a signal of -128 dBm printed as though it had been measured
/// sends whoever is reading it after a hardware problem that is not there.
#[test]
fn a_failure_without_a_signal_does_not_invent_one() {
    let failure = JoinFailure {
        reason: Reason::NoSuchNetwork,
        signal: None,
    };

    assert_eq!(render(&failure), "nothing with that name was heard");
}

/// Two reasons and no signal print as two sentences and nothing else, which is the shape a reader of
/// the serial output sees when the radio never measured anything.
#[test]
fn a_failure_without_a_signal_keeps_the_reason_verbatim() {
    let failure = JoinFailure {
        reason: Reason::SecurityRefused,
        signal: None,
    };

    assert_eq!(
        render(&failure),
        render(&Reason::SecurityRefused),
        "a failure with no signal should read as its reason does",
    );
}

/// The time of day is written the way a person writes it, which is three two-digit fields and a colon
/// between each of them.
#[test]
fn a_clock_is_written_as_a_time_of_day() {
    assert_eq!(render(&Clock::since_boot(0)), "00:00:00");
    assert_eq!(render(&Clock::since_boot(45)), "00:00:45");
    assert_eq!(render(&Clock::since_boot(3_661)), "01:01:01");
}

/// Every field is two digits wide, always: the greeting prints twice a second, and a reading whose
/// shape changes with its value is a log that is hard to read and harder to grep.
#[test]
fn a_clock_pads_every_field_to_two_digits() {
    assert_eq!(render(&Clock::since_boot(3_723)), "01:02:03");
    assert_eq!(render(&Clock::since_boot(86_399)), "23:59:59");
}

/// The chip's clock starts at zero when the firmware starts, so a day of running time has to print as
/// zero rather than as 86.401, and the largest number of seconds there is still has to land inside a
/// day. Wrapping is what makes the reading a shape rather than a measurement — it is wrong until
/// there is a network time source, and pretending otherwise would be the part that misleads.
#[test]
fn a_clock_counting_from_boot_wraps_at_midnight() {
    assert_eq!(render(&Clock::since_boot(SECONDS_PER_DAY)), "00:00:00");
    assert_eq!(render(&Clock::since_boot(2 * SECONDS_PER_DAY)), "00:00:00");
    assert_eq!(
        render(&Clock::since_boot(SECONDS_PER_DAY + 3_723)),
        "01:02:03"
    );

    // The largest count of seconds there is, which is not a round number of days and so lands
    // wherever the arithmetic puts it — as long as it lands inside the day.
    assert_eq!(render(&Clock::since_boot(u64::MAX)), "07:00:15");
}

/// A real time carries the date, and not only because it is interesting: a clock that cannot say
/// which day its hours belong to is half a clock, and the date is also what makes a reading that is
/// a century out visible instead of plausible.
#[test]
fn a_synchronized_clock_carries_the_date() {
    // The epoch itself, and the second after it: the boundary every count of seconds is measured
    // from, and the one an off-by-one shows up on.
    assert_eq!(render(&Clock::utc(0)), "1970-01-01 00:00:00");
    assert_eq!(render(&Clock::utc(1)), "1970-01-01 00:00:01");
}

/// The last second of a day and the first of the next, in a year with a leap day in it: a count of
/// seconds that is one out at the boundary is a clock that is wrong for an hour a day rather than
/// one that is obviously broken.
#[test]
fn the_boundaries_of_a_day_are_where_they_are() {
    let midnight = epoch_secs_of(2026, 10, 5);

    assert_eq!(render(&Clock::utc(midnight - 1)), "2026-10-04 23:59:59");
    assert_eq!(render(&Clock::utc(midnight)), "2026-10-05 00:00:00");
    assert_eq!(
        render(&Clock::utc(midnight + SECONDS_PER_DAY - 1)),
        "2026-10-05 23:59:59"
    );
}

/// February is 29 days in a leap year and 28 in the rest, and 2100 is the century that is divisible
/// by four and is not a leap year. Those three facts are the whole of what a calendar gets wrong, so
/// they are the three that are pinned: the day either side of each is the assertion, because a
/// month length written into the arithmetic twice shows up as a date that is one day out.
#[test]
fn february_follows_the_leap_year_rule() {
    assert_eq!(
        render(&Clock::utc(epoch_secs_of(2024, 2, 29))),
        "2024-02-29 00:00:00"
    );
    assert_eq!(
        render(&Clock::utc(epoch_secs_of(2024, 3, 1))),
        "2024-03-01 00:00:00"
    );

    // A century divisible by 400 is a leap year; one that is only divisible by four is not.
    assert_eq!(
        render(&Clock::utc(epoch_secs_of(2000, 2, 29))),
        "2000-02-29 00:00:00"
    );
    assert_eq!(
        render(&Clock::utc(epoch_secs_of(2100, 3, 1))),
        "2100-03-01 00:00:00"
    );
}

/// Every month of a year, which is the property that a month length is 30 or 31 and not 31 for all
/// of them. One table rather than twelve tests, because the property is that the set is covered.
#[test]
fn every_month_has_the_length_it_has() {
    const LENGTHS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    let mut first = Clock::utc(epoch_secs_of(2025, 1, 1));

    for (index, length) in LENGTHS.iter().enumerate() {
        let month = index as u64 + 1;

        assert!(
            render(&first).starts_with(&format!("2025-{month:02}-01")),
            "the first of month {month} rendered as {}",
            render(&first),
        );

        // The last day of the month is the one the table names, and the first of the next month is
        // the day after it — which is the whole claim being made here: the two are one day apart.
        let last = Clock::utc(epoch_secs_of(2025, month, *length));
        assert!(
            render(&last).starts_with(&format!("2025-{month:02}-{length:02}")),
            "the last of month {month} rendered as {}",
            render(&last),
        );

        first = Clock::utc(epoch_secs_of(2025, month, *length) + SECONDS_PER_DAY);
    }

    assert_eq!(
        render(&first),
        "2026-01-01 00:00:00",
        "after December comes January"
    );
}

/// A count of seconds no calendar has is not a time any server would send, but it is one the firmware
/// can be handed — from a packet that passed every other check — and printing it has to produce a
/// date rather than an overflow panic. A panic here would be in the greeting, twice a second, and it
/// would take the firmware down.
#[test]
fn an_impossible_epoch_renders_rather_than_overflowing() {
    // The largest count there is, which in a debug build is where an unchecked addition would panic
    // rather than wrap. The year is absurd; the point is that the arithmetic gets to the formatting.
    let absurd = render(&Clock::utc(u64::MAX));

    assert!(
        absurd.starts_with("584"),
        "the year is finite even though the date is absurd: {absurd}"
    );
    assert!(
        absurd.contains('-') && absurd.contains(':'),
        "a date and a time rather than a panic: {absurd}"
    );
}

/// Days counted between two dates rather than one date at a time, because a wrong year length shows
/// up as a whole day out and nothing else: 1970-01-01 to 2000-01-01 is 10 957 days, and 2000-01-01
/// to 2026-10-04 is 9 773.
#[test]
fn days_are_counted_between_dates() {
    assert_eq!(
        render(&Clock::utc(10_957 * SECONDS_PER_DAY)),
        "2000-01-01 00:00:00"
    );

    let two_thousand = 10_957 * SECONDS_PER_DAY;
    let today = two_thousand + 9_773 * SECONDS_PER_DAY;

    assert_eq!(render(&Clock::utc(today)), "2026-10-04 00:00:00");
}

/// Seconds since the epoch for a date, written the slow way.
///
/// This is the one piece of arithmetic in this file that does not use the implementation under test:
/// the days in each month and the leap-year rule applied by hand, so that a mistake in
/// `poc_report::Clock` cannot also be present in the thing that checks it.
fn epoch_secs_of(year: u64, month: u64, day: u64) -> u64 {
    const LENGTHS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    let mut days = 0;
    for past in 1970..year {
        days += 365 + u64::from(past % 4 == 0 && (past % 100 != 0 || past % 400 == 0));
    }
    for length in LENGTHS.iter().take(month as usize - 1) {
        days += length;
    }
    days += u64::from(month > 2 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0));
    days += day - 1;

    days * SECONDS_PER_DAY
}
