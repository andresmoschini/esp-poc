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
        (
            Reason::HandshakeStalled,
            "the handshake started and did not finish",
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

/// The named reasons are different problems, and two failures that read alike are the two that most
/// often get confused: a wrong password and a station that is too far away both arrive as an exchange
/// that stops. If these ever collapse into one sentence, the distinction that matters on the board is
/// gone.
///
/// `HandshakeStalled` is here rather than folded into `SecurityRefused` for the same reason. Both of
/// those two failures arrive as a handshake that did not finish, and the one thing the radio did not
/// say was which of the two it was.
#[test]
fn the_reasons_are_distinguishable() {
    let sentences = [
        Reason::NoSuchNetwork,
        Reason::SecurityRefused,
        Reason::NoAnswer,
        Reason::HandshakeStalled,
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

/// A real time is the count of seconds since the epoch, printed as the number: the date is what
/// the reported event's timestamp is for, and the greeting that carries this one is read twice a
/// second. The calendar behind the timestamp is pinned where the timestamp is tested.
#[test]
fn a_synchronized_clock_is_the_count_of_seconds() {
    assert_eq!(render(&Clock::utc(0)), "0");
    // 2026-10-04T18:22:31Z, the moment the other test files write lines at.
    assert_eq!(render(&Clock::utc(1_791_138_151)), "1791138151");
}
