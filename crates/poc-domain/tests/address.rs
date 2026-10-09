// The tests for how an address is written.
//
// They live in `tests/` rather than in a `#[cfg(test)]` module because this crate is `#![no_std]`:
// an integration test is a separate crate with the standard prelude, so `assert_eq!` and `String` are
// there without the library giving up `no_std` for its own build. They run on the machine running the
// gate, which is the whole reason this crate exists — the rest of the firmware cannot be compiled for
// a host at all, and cannot be run anywhere CI has.

use std::fmt::Write as _;

use poc_domain::Address;

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
