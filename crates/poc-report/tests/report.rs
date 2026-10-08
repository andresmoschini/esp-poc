// The tests for the sentences the firmware prints.
//
// They live in `tests/` rather than in a `#[cfg(test)]` module because this crate is `#![no_std]`:
// an integration test is a separate crate with the standard prelude, so `assert_eq!` and `String` are
// there without the library giving up `no_std` for its own build. They run on the machine running the
// gate, which is the whole reason this crate exists — the rest of the firmware cannot be compiled for
// a host at all, and cannot be run anywhere CI has.

use std::fmt::Write as _;

use poc_report::{Address, Clock};

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

/// A real time is the count of seconds since the epoch, printed as the number: the date is what
/// the reported event's timestamp is for, and the greeting that carries this one is read twice a
/// second. The calendar behind the timestamp is pinned where the timestamp is tested.
#[test]
fn a_synchronized_clock_is_the_count_of_seconds() {
    assert_eq!(render(&Clock::utc(0)), "0");
    // 2026-10-04T18:22:31Z, the moment the other test files write lines at.
    assert_eq!(render(&Clock::utc(1_791_138_151)), "1791138151");
}
