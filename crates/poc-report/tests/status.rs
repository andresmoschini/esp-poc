// The tests for what the firmware says about itself: what the radio is doing, where the time came
// from, and the line the two of them are printed on.
//
// They are here in `tests/` for the reason `tests/report.rs` opens with, and they cover the same
// ground from the other side. That file checks how a value reads; this one checks how a state is
// chosen and how the parts of the line are ordered — which is the half of the greeting that
// decides rather than spells.
//
// Two of the types here exist only to be published from one task and read in another ([`Link`] and
// [`Obstruction`]), behind a lock each rather than as one word: what the lock holds is the value
// itself, so there is no encoding for a test to round-trip and the sentences are what is pinned.

use std::fmt::Write as _;

use poc_report::{
    Address, JoinFailure, Link, Obstruction, Reason, Refusal, STALE_AFTER_SECS, Status, Time, age,
};

/// 2026-10-04T18:22:31Z, the moment every line below is written at.
///
/// Written as the arithmetic it is rather than as the number, because a count of seconds with no
/// date beside it is one nobody can check: 20 730 days are 1970-01-01 to 2026-10-04 and the rest is
/// the time of day. Every assertion below renders it, so a wrong value here fails loudly.
const AT: u64 = 20_730 * 24 * 60 * 60 + 18 * 3_600 + 22 * 60 + 31;

/// Renders a value the way the firmware's `Display2Format` does on the chip.
///
/// The same helper `tests/report.rs` opens with, for the same reason: `core::fmt` is the same code
/// in both places, so this is not an approximation of what the board prints.
fn render(value: &impl std::fmt::Display) -> String {
    let mut written = String::new();

    write!(written, "{value}").expect("writing to a String cannot fail");

    written
}

/// The line the firmware prints once everything is working, in full.
///
/// Pinned verbatim and not by its parts, because the whole of this is the order: a reader who came
/// to see the time has it in the first field, a reader who came to see the address has it in the
/// next, and the state of both is last because it is what is read when one of the first two is wrong.
/// Reordering those three is a change to the log that no other test here would notice.
#[test]
fn the_state_line_reads_the_time_then_the_address_then_the_radio() {
    let status = Status {
        time: Time::answered(AT, 2, 30),
        link: Link::Joined,
        address: Some(Address {
            ip: "192.168.0.225".parse().unwrap(),
            prefix_len: 24,
        }),
    };

    assert_eq!(
        render(&status),
        "1791138151 UTC (from a stratum 2 server), 192.168.0.225/24, wifi: joined",
    );
}

/// The state a gate build and a CI build are in: no credentials, so no network, so no address, and a
/// clock counting from boot. Nothing has gone wrong, and every part of the line says so rather than
/// leaving a reader to work backwards from three absences.
#[test]
fn a_build_with_no_credentials_says_it_had_nothing_to_join() {
    let status = Status {
        time: Time::since_boot(7),
        link: Link::NoNetwork,
        address: None,
    };

    assert_eq!(
        render(&status),
        "00:00:07 (counting from boot: nothing has answered yet), no address yet, \
         wifi: nothing to join: no credentials were compiled in",
    );
}

/// A missing address is said rather than left out. An absent clause reads as a formatting mistake,
/// and the state of the radio is the answer to it — but the clause that names the absence is what
/// stops it reading as a mistake at all.
#[test]
fn a_line_without_an_address_accounts_for_it() {
    let status = Status {
        time: Time::since_boot(42),
        link: Link::Joining,
        address: None,
    };

    assert_eq!(
        render(&status),
        "00:00:42 (counting from boot: nothing has answered yet), no address yet, wifi: joining",
    );
}

/// Both mechanisms failed, both with a reason, and the line carries both. This is the case the whole
/// state line exists for: a chip that is not on a network and cannot be told the time, read by
/// someone who has to decide which of the two to go and look at.
#[test]
fn a_failed_join_and_a_lost_time_both_say_why() {
    let status = Status {
        time: Time::since_boot_after(63, Obstruction::AnswerTimedOut),
        link: Link::Failed(JoinFailure {
            reason: Reason::NoSuchNetwork,
            signal: Some(-81),
        }),
        address: None,
    };

    assert_eq!(
        render(&status),
        "00:01:03 (counting from boot: the time server's answer did not arrive), no address yet, \
         wifi: not joined: nothing with that name was heard (signal -81 dBm)",
    );
}

/// A clock that a server set and nobody has confirmed since is drifting on its own crystal, and
/// that is worth more than the fact that it once had a time. The age is here because "not recently"
/// is a thing a reader has to be told the size of.
#[test]
fn a_time_nobody_has_confirmed_recently_says_how_long_ago() {
    let status = Status {
        time: Time::answered(AT, 3, STALE_AFTER_SECS + 5 * 60),
        link: Link::Joined,
        address: None,
    };

    assert_eq!(
        render(&status),
        "1791138151 UTC (from a stratum 3 server, last confirmed 3900s ago), \
         no address yet, wifi: joined",
    );
}

/// The second at which an answer stops being current, and the second before it. The threshold is
/// `STALE_AFTER_SECS` because that is how often `src/ntp.rs` asks again: an answer younger than one
/// resync interval means the clock has been corrected since the link was last checked. Both ends are
/// pinned because a boundary that is off by one is a clock that calls itself confirmed for a minute
/// after it stopped being confirmed, which is the failure nobody sees.
#[test]
fn the_second_an_answer_goes_stale_is_the_second_it_stops_being_current() {
    let fresh = render(&Time::answered(AT, 2, STALE_AFTER_SECS - 1));
    let stale = render(&Time::answered(AT, 2, STALE_AFTER_SECS));

    assert_eq!(
        fresh, "1791138151 UTC (from a stratum 2 server)",
        "an answer one second short of the resync interval is still current",
    );
    assert_eq!(
        stale,
        "1791138151 UTC (from a stratum 2 server, last confirmed 3600s ago)",
    );
}

/// The shape of the two times is what says which one a reading is, and it is what has been relied on
/// since before there was a state line: a bare `HH:MM:SS` is the chip counting from boot, and a
/// reading with a date came from a server. Nothing here can hold `Time` to that — a caller could put
/// any two numbers in — so it is pinned as a property of what this crate writes.
#[test]
fn a_time_from_boot_has_no_date_and_a_time_from_a_server_does() {
    assert_eq!(
        render(&Time::since_boot(42)),
        "00:00:42 (counting from boot: nothing has answered yet)",
    );

    assert_eq!(
        render(&Time::answered(AT, 2, 30)),
        "1791138151 UTC (from a stratum 2 server)",
    );
}

/// Every state of the link says something a reader can act on, and the set is covered rather than
/// sampled: a variant added to the enum without a sentence here fails to compile, which is the
/// point. They are pinned verbatim because the sentence is the whole contribution this firmware
/// makes to a log full of driver output.
#[test]
fn every_link_has_a_sentence() {
    let expected = [
        (
            Link::NoNetwork,
            "nothing to join: no credentials were compiled in",
        ),
        (
            Link::UnusableCredential,
            "nothing to join: a compiled-in credential is not one the radio can use",
        ),
        (Link::NoRadio, "nothing to join: the radio did not start"),
        (Link::Joining, "joining"),
        (Link::Joined, "joined"),
        (
            Link::Failed(JoinFailure {
                reason: Reason::SecurityRefused,
                signal: None,
            }),
            "not joined: the network refused these credentials",
        ),
    ];

    for (link, sentence) in expected {
        assert_eq!(render(&link), sentence, "sentence for {link:?}");
    }
}

/// The states with no network in them are three, not one, because a reader who cannot tell them
/// apart does not know which file to open: there is nothing to type, something to fix, or a board to
/// replace. Two of them collapsing into one sentence is the mistake this pins.
#[test]
fn the_three_ways_of_having_no_network_are_distinguishable() {
    let sentences =
        [Link::NoNetwork, Link::UnusableCredential, Link::NoRadio].map(|link| render(&link));

    let mut unique = sentences.to_vec();
    unique.sort_unstable();
    unique.dedup();

    assert_eq!(unique.len(), sentences.len(), "two states read the same");
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
