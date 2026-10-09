// The tests for what stands between the chip and a time, and for how long an answer stays current.
///
// They are here in `tests/` for the reason `tests/report.rs` opens with, and they cover the half of
// the greeting that decides rather than spells: the clock's last failure and the age of its last
// answer. The state line as a whole lives in `src/status.rs` now, next to the radio and the stack
// it is assembled from, and a test on it needs a board.
use std::fmt::Write as _;

use poc_report::{Obstruction, Refusal, age};

/// Renders a value the way the firmware's `Display2Format` does on the chip.
///
/// The same helper `tests/report.rs` opens with, for the same reason: `core::fmt` is the same code
/// in both places, so this is not an approximation of what the board prints.
fn render(value: &impl std::fmt::Display) -> String {
    let mut written = String::new();

    write!(written, "{value}").expect("writing to a String cannot fail");

    written
}

/// Every obstruction has a sentence, and the three timeouts are three of them rather than one
/// carrying the name of the step: a name that does not resolve and a server that does not answer
/// want opposite fixes, and "timed out" alone sends whoever is reading it after the network.
#[test]
fn every_obstruction_has_a_sentence() {
    let expected = [
        (
            Obstruction::LookupTimedOut,
            "the name of the time server did not resolve in time",
        ),
        (
            Obstruction::RequestTimedOut,
            "the request did not reach the time server",
        ),
        (
            Obstruction::AnswerTimedOut,
            "the time server's answer did not arrive",
        ),
        (
            Obstruction::NoServer,
            "the name of the time server did not resolve to an address",
        ),
        (
            Obstruction::Stranger,
            "the reply came from an address that was not asked",
        ),
        (
            Obstruction::WouldNotSend,
            "the socket would not send the request",
        ),
        (
            Obstruction::TooLong,
            "the reply was larger than the receive buffer",
        ),
        (
            Obstruction::Refused(Refusal::KissOfDeath),
            "the server will not answer this client",
        ),
    ];

    for (obstruction, sentence) in expected {
        assert_eq!(
            render(&obstruction),
            sentence,
            "sentence for {obstruction:?}"
        );
    }
}

/// Two obstructions that read alike are two that will be confused, and this set has one case per
/// thing that can go wrong between this chip and a server. The three timeouts are here as three.
#[test]
fn the_obstructions_are_distinguishable() {
    let sentences = [
        Obstruction::LookupTimedOut,
        Obstruction::RequestTimedOut,
        Obstruction::AnswerTimedOut,
        Obstruction::NoServer,
        Obstruction::Stranger,
        Obstruction::WouldNotSend,
        Obstruction::TooLong,
    ]
    .map(|obstruction| render(&obstruction));

    let mut unique = sentences.to_vec();
    unique.sort_unstable();
    unique.dedup();

    assert_eq!(
        unique.len(),
        sentences.len(),
        "two obstructions read the same"
    );
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
