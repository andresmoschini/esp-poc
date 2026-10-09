// The tests for how the firmware's clock is written.
//
// They live in `tests/` rather than in a `#[cfg(test)]` module because this crate is `#![no_std]`:
// an integration test is a separate crate with the standard prelude, so `assert_eq!` and `String` are
// there without the library giving up `no_std` for its own build. They run on the machine running the
// gate, which is the whole reason this crate exists — the rest of the firmware cannot be compiled for
// a host at all, and cannot be run anywhere CI has.

use std::fmt::Write as _;

use poc_domain::{Clock, STALE_AFTER_SECS, Setting, Source, Time, Timestamp, age, source};

/// 2026-10-04T18:22:31Z, written as the arithmetic so the number is not produced by the code
/// under test.
const AT: u64 = 20_730 * 24 * 60 * 60 + 18 * 3_600 + 22 * 60 + 31;

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

/// A real time is RFC 3339 in UTC, the same rendering as the reported event's timestamp: one
/// calendar shape for the serial log and the stored row, so a greeting reads the same as what the
/// API kept. The calendar itself is pinned where the timestamp is tested.
#[test]
fn a_synchronized_clock_is_rfc_3339() {
    assert_eq!(render(&Clock::utc(0)), "1970-01-01T00:00:00Z");
    // 2026-10-04T18:22:31Z, the moment the other test files write lines at.
    assert_eq!(render(&Clock::utc(1_791_138_151)), "2026-10-04T18:22:31Z");
}

/// An age is written as a count of seconds: "45s", "3600s". A count is nothing to format beyond
/// the number, and the largest count there is still just digits — which in a debug build is where
/// an unchecked arithmetic would panic rather than wrap.
#[test]
fn an_age_is_written_in_seconds() {
    let expected = [
        (0, "0s"),
        (1, "1s"),
        (45, "45s"),
        (60, "60s"),
        (3_661, "3661s"),
        (3 * 24 * 60 * 60 + 4 * 60 * 60, "273600s"),
        (u64::MAX, "18446744073709551615s"),
    ];

    for (secs, written) in expected {
        assert_eq!(render(&age(secs)), written, "an age of {secs} seconds");
    }
}

/// into the body is the decision this test pins.
#[test]
fn the_timestamp_is_rfc_3339() {
    assert_eq!(render(&Timestamp::at(AT)), "2026-10-04T18:22:31Z");

    // Both ends of the day, and the leap day itself: a timestamp that rolls over wrongly is a row
    // that sorts into the wrong hour rather than an error anybody sees.
    assert_eq!(render(&Timestamp::at(0)), "1970-01-01T00:00:00Z");
    assert_eq!(
        render(&Timestamp::at(20_730 * 24 * 60 * 60 - 1)),
        "2026-10-03T23:59:59Z"
    );
    assert_eq!(
        render(&Timestamp::at(20_730 * 24 * 60 * 60)),
        "2026-10-04T00:00:00Z"
    );
}

/// stores happily and a reader cannot query.
#[test]
fn the_timestamp_handles_the_ends_of_a_leap_year() {
    // 2028-02-29T12:00:00Z: a leap day in a year divisible by four.
    assert_eq!(
        render(&Timestamp::at(1_835_438_400)),
        "2028-02-29T12:00:00Z",
    );

    // 2000-02-29T00:00:00Z and the day after it: a century divisible by 400 is a leap year, so
    // February has 29 days rather than 28.
    assert_eq!(render(&Timestamp::at(951_782_400)), "2000-02-29T00:00:00Z",);
    assert_eq!(render(&Timestamp::at(951_868_800)), "2000-03-01T00:00:00Z",);

    // 2100-02-28T23:59:59Z, the last second before a century that is divisible by 100 and not by
    // 400 takes its leap day away. A calendar that divides by four here is wrong once every hundred
    // years, which is exactly the sort of thing that is right in every test somebody writes.
    assert_eq!(
        render(&Timestamp::at(4_107_542_399)),
        "2100-02-28T23:59:59Z"
    );
}

/// arithmetic twice shows up as a date that is one day out.
#[test]
fn every_month_has_the_length_it_has() {
    const LENGTHS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

    // 2025-01-01T00:00:00Z, and 2025 is not a leap year, so February below is 28 days.
    let mut first = 1_735_689_600;

    for (index, length) in LENGTHS.iter().enumerate() {
        let month = index as u64 + 1;

        assert_eq!(
            render(&Timestamp::at(first)),
            format!("2025-{month:02}-01T00:00:00Z"),
            "the first of month {month}",
        );

        // The last day of the month is the one the table names, and the first of the next month is
        // the day after it — which is the whole claim being made here: the two are one day apart.
        let last = first + (length - 1) * 86_400;

        assert_eq!(
            render(&Timestamp::at(last)),
            format!("2025-{month:02}-{length:02}T00:00:00Z"),
            "the last of month {month}",
        );

        first = last + 86_400;
    }

    assert_eq!(
        render(&Timestamp::at(first)),
        "2026-01-01T00:00:00Z",
        "after December comes January"
    );
}

/// and it would take the report down.
#[test]
fn an_impossible_epoch_renders_rather_than_overflowing() {
    // The largest count there is, which in a debug build is where an unchecked addition would panic
    // rather than wrap. The year is absurd; the point is that the arithmetic gets to the formatting.
    let absurd = render(&Timestamp::at(u64::MAX));

    assert!(
        absurd.starts_with("584"),
        "the year is finite even though the date is absurd: {absurd}"
    );
    assert!(
        absurd.contains('T') && absurd.contains('Z') && absurd.contains(':'),
        "a timestamp rather than a panic: {absurd}"
    );
}

// --- The arithmetic behind it ----------------------------------------------------

/// A time a server would hand over: 2026-10-04T18:22:31Z.
const A_DATE: u64 = 1_791_138_151;

/// The clock as `src/clock.rs` stores it when a server answers.
fn set_to(epoch_secs: u64, stratum: u8, answered_at_secs: u64) -> Setting {
    Setting {
        epoch_secs,
        stratum,
        answered_at_secs,
    }
}

/// With no answer there is nothing a source is made of, and `src/clock.rs` reads that as the clock
/// counting from boot rather than as an error.
#[test]
fn no_answer_has_no_source() {
    assert_eq!(source(None, 42), None);
}

/// The instant the answer arrived is the instant the time is right: nothing has been counted on yet,
/// so the age is zero and the epoch is the server's own number unchanged.
#[test]
fn an_answer_read_at_the_moment_it_arrives_is_unchanged() {
    let answer = set_to(A_DATE, 2, 100);

    assert_eq!(
        source(Some(answer), 100),
        Some(Source {
            epoch_secs: A_DATE,
            stratum: 2,
            age_secs: 0,
        })
    );
}

/// The whole of the clock's job: the chip has no time of its own, so the time is the server's plus
/// everything counted since. Both halves of the reading come from the same subtraction, which is
/// why they cannot drift apart.
#[test]
fn the_time_moves_on_by_exactly_the_age() {
    for elapsed in [0, 1, 60, 3_723, STALE_AFTER_SECS, 86_400] {
        let answer = set_to(A_DATE, 3, 100);

        let reading = source(Some(answer), 100 + elapsed).expect("there is an answer");

        assert_eq!(reading.age_secs, elapsed);
        assert_eq!(reading.epoch_secs, A_DATE + elapsed);
    }
}

/// An answer set at boot and read a day later is a day of the chip's own counting, not an hour: the
/// age is measured against when the answer arrived, not against when the firmware started.
#[test]
fn the_age_is_measured_from_the_answer_not_from_boot() {
    let answer = set_to(A_DATE, 1, 3_600);

    assert_eq!(
        source(Some(answer), 4_600)
            .expect("there is an answer")
            .age_secs,
        1_000,
    );
}

/// The stratum travels unchanged, whatever else has happened to the time since.
#[test]
fn the_stratum_is_reported_as_the_server_gave_it() {
    for stratum in [1, 2, 15] {
        let answer = set_to(A_DATE, stratum, 0);

        assert_eq!(
            source(Some(answer), 500)
                .expect("there is an answer")
                .stratum,
            stratum,
        );
    }
}

/// A counter that has gone backwards — a wrap, a suspend that was not a suspend — must not produce
/// a negative age, and `u64` has no negative, so the only wrong answer available is a large one.
/// Saturating gives zero, which is a reading of "the answer just arrived".
#[test]
fn a_counter_that_went_backwards_does_not_wrap() {
    let answer = set_to(A_DATE, 2, 100);

    let reading = source(Some(answer), 10).expect("there is an answer");

    assert_eq!(reading.age_secs, 0);
    assert_eq!(reading.epoch_secs, A_DATE);
}

/// The same for the date: an epoch near the end of what a `u64` holds saturates rather than wrapping
/// to a time before 1970, which would be a reading of the past from a server reporting the future.
#[test]
fn an_epoch_that_would_overflow_saturates() {
    let answer = set_to(u64::MAX - 10, 2, 0);

    let reading = source(Some(answer), 1_000).expect("there is an answer");

    assert_eq!(reading.epoch_secs, u64::MAX);
    assert_eq!(reading.age_secs, 1_000);
}

/// The largest age there is is still an age, and the largest date there is still renders — in a debug
/// build, which is where an unchecked addition panics rather than quietly wrapping.
#[test]
fn the_extremes_of_both_do_not_panic() {
    let answer = set_to(u64::MAX, 2, 0);

    let reading = source(Some(answer), u64::MAX).expect("there is an answer");

    assert_eq!(reading.age_secs, u64::MAX);
    assert_eq!(reading.epoch_secs, u64::MAX);

    // And the sentence the greeting would print from it is a sentence rather than a panic.
    let rendered = Time::answered(reading.epoch_secs, reading.stratum, reading.age_secs);

    assert!(rendered.to_string().contains(&age(u64::MAX).to_string()));
}
