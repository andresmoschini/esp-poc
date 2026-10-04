// The tests for what the firmware says about itself: what the radio is doing, where the time came
// from, and the line the two of them are printed on.
//
// They are here in `tests/` for the reason `tests/report.rs` opens with, and they cover the same
// ground from the other side. That file checks how a value reads; this one checks how a state is
// chosen, how it is published for another task to read, and how the parts of the line are ordered —
// which is the half of the greeting that decides rather than spells.
//
// Two of the types here exist only to be published from one task and read in another ([`Link`] and
// [`Obstruction`]), so the round trips through their words are the tests that matter most here: a
// sentence that reads well beside a word that means something else is precisely the failure a board
// would not show anybody, because the board has one value and no way to compare it with the one that
// was meant.

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
        "2026-10-04 18:22:31 UTC (from a stratum 2 server), 192.168.0.225/24, wifi: joined",
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
        "2026-10-04 18:22:31 UTC (from a stratum 3 server, last confirmed 1 hour 5 minutes ago), \
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
        fresh, "2026-10-04 18:22:31 UTC (from a stratum 2 server)",
        "an answer one second short of the resync interval is still current",
    );
    assert_eq!(
        stale,
        "2026-10-04 18:22:31 UTC (from a stratum 2 server, last confirmed 1 hour ago)",
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
        "2026-10-04 18:22:31 UTC (from a stratum 2 server)",
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
        (Link::Unknown, "in a state this firmware does not name"),
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

/// A link is published as one word for another task to read, so every state has to survive being
/// written and read back. This is the test that the words and the enum mean the same thing, and it is
/// the one that fails when a variant is added to the enum and its tag is not.
#[test]
fn every_link_survives_being_published() {
    let links = [
        Link::NoNetwork,
        Link::UnusableCredential,
        Link::NoRadio,
        Link::Joining,
        Link::Joined,
        Link::Failed(JoinFailure {
            reason: Reason::LinkLost,
            signal: None,
        }),
        Link::Unknown,
    ];

    for link in links {
        assert_eq!(
            Link::from_word(link.to_word()),
            link,
            "the word for {link:?} read back as something else",
        );
    }
}

/// A failure carries two fields behind the one tag, and each has to survive the trip on its own: the
/// reason is what to do about it and the signal is what tells two reasons apart, so a word that kept
/// one and lost the other would be a log that sends the wrong person to the wrong place.
#[test]
fn every_reason_survives_being_published() {
    let reasons = [
        Reason::NoSuchNetwork,
        Reason::SecurityRefused,
        Reason::NoAnswer,
        Reason::LinkLost,
        Reason::Other,
    ];

    for reason in reasons {
        let link = Link::Failed(JoinFailure {
            reason,
            signal: None,
        });

        assert_eq!(
            Link::from_word(link.to_word()),
            link,
            "the word for {reason:?} read back as something else",
        );
    }
}

/// Every dBm value a radio can report, and the absence of one. The driver fills in -128 when it has
/// no reading, and `src/wifi.rs` maps that to `None` before publishing — so the interesting case is
/// that `None` and a real reading are different words, which is what the next test says and what this
/// one has to make true of every value.
#[test]
fn every_signal_a_radio_can_report_survives_being_published() {
    for dbm in i8::MIN..=i8::MAX {
        let link = Link::Failed(JoinFailure {
            reason: Reason::NoAnswer,
            signal: Some(dbm),
        });

        assert_eq!(
            Link::from_word(link.to_word()),
            link,
            "a signal of {dbm} dBm read back as something else",
        );
    }

    let unmeasured = Link::Failed(JoinFailure {
        reason: Reason::NoAnswer,
        signal: None,
    });

    assert_eq!(
        Link::from_word(unmeasured.to_word()),
        unmeasured,
        "a failure with no measurement read back as one that has",
    );
}

/// 0 dBm is a reading — a very loud one, at the edge of what a radio reports — and "none" is not a
/// reading. They are the same byte if the word encodes one as zero, so the word carries a byte saying
/// whether the one beside it is a reading at all. A firmware that spent the byte and lost this would
/// print "-128 dBm" or "0 dBm" as though a station had measured it, which is a contradiction a
/// reader would go and look for in the hardware.
#[test]
fn zero_dbm_is_not_the_absence_of_a_signal() {
    let loud = Link::Failed(JoinFailure {
        reason: Reason::LinkLost,
        signal: Some(0),
    });

    let unmeasured = Link::Failed(JoinFailure {
        reason: Reason::LinkLost,
        signal: None,
    });

    assert_ne!(
        loud.to_word(),
        unmeasured.to_word(),
        "0 dBm and no signal share a word"
    );
    assert_eq!(Link::from_word(loud.to_word()), loud);
    assert_eq!(Link::from_word(unmeasured.to_word()), unmeasured);
}

/// A word this build did not write reads as the state for a word this build did not write, rather
/// than as whichever state happens to be first or as a panic. Every tag in the gap is walked, because
/// the gap is the whole of what another build could have written: a newer one adds its states above
/// `Unknown` and leaves this build reading a tag it does not know.
#[test]
fn a_word_no_build_of_this_firmware_wrote_is_a_state_this_build_does_not_name() {
    for tag in 6u8..=254 {
        assert_eq!(
            Link::from_word(u32::from(tag)),
            Link::Unknown,
            "the tag {tag} read as a state this build has",
        );
    }

    // The all-ones word is what an uninitialized or clobbered word looks like most often, and it is
    // `Unknown`'s own tag, so it is the one case that must not read as anything else.
    assert_eq!(Link::from_word(u32::MAX), Link::Unknown);
}

/// A failure written by a build with a different idea of the rest of the word is still a failure. This
/// is the case where the tag is understood and the bytes behind it are not, and it matters because
/// the alternative is `Unknown` — which reads as a state the radio is not in.
#[test]
fn a_failed_join_this_build_cannot_read_all_the_way_is_still_a_failed_join() {
    // The tag of a failure, with reason and signal bytes no build of this firmware writes.
    let word = Link::Failed(JoinFailure {
        reason: Reason::Other,
        signal: None,
    })
    .to_word()
        | 0x00FF_FF00;

    match Link::from_word(word) {
        Link::Failed(failure) => {
            assert_eq!(
                failure.reason,
                Reason::Other,
                "a reason no build wrote is not named"
            );
            assert_eq!(
                failure.signal, None,
                "a byte that is not a measurement is not one"
            );
        }
        other => panic!("a failed join read as {other:?}"),
    }
}

/// The bytes behind the tag are the spare half of the word, and a state that does not use them must
/// not care what is in them. Otherwise a word written by a newer build would read as `Unknown` in
/// states where the tag alone says everything there is to say.
#[test]
fn the_spare_bytes_of_a_word_do_not_change_what_it_means() {
    let word = Link::Joined.to_word() | 0x00FF_FF00;

    assert_eq!(Link::from_word(word), Link::Joined);
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

/// An obstruction is published from the task that asks a server and read by the task that keeps the
/// clock, so it has to survive being written and read back — and the refusal inside one has to
/// survive with it, or a bad packet would be reported as some other bad packet.
#[test]
fn every_obstruction_survives_being_published() {
    let obstructions = [
        Obstruction::LookupTimedOut,
        Obstruction::RequestTimedOut,
        Obstruction::AnswerTimedOut,
        Obstruction::NoServer,
        Obstruction::Stranger,
        Obstruction::WouldNotSend,
        Obstruction::TooLong,
    ];

    for obstruction in obstructions {
        assert_eq!(
            Obstruction::from_word(obstruction.to_word()),
            Some(obstruction),
            "the word for {obstruction:?} read back as something else",
        );
    }
}

/// Every refusal a server's packet can be refused for, carried whole through the word. This is the
/// one case where an obstruction is a pair rather than a single value, and it is where a word with
/// room for one byte and a need for two would quietly lose which of the eight happened.
#[test]
fn every_refusal_survives_being_published_inside_an_obstruction() {
    let refusals = [
        Refusal::Short,
        Refusal::NotAReply,
        Refusal::Version,
        Refusal::KissOfDeath,
        Refusal::NotAServer,
        Refusal::Unsynchronized,
        Refusal::NotOurs,
        Refusal::NoTime,
        Refusal::BeforeTheEpoch,
    ];

    for refusal in refusals {
        let obstruction = Obstruction::Refused(refusal);

        assert_eq!(
            Obstruction::from_word(obstruction.to_word()),
            Some(obstruction),
            "the word for {refusal:?} read back as something else",
        );
    }
}

/// Before the first attempt there is nothing to explain, and that is a word rather than a variant:
/// it is the word the clock's static starts as, and a build that cannot read a word at all has
/// nothing to say about a time it never asked for.
#[test]
fn no_attempt_yet_publishes_as_nothing_at_all() {
    assert_eq!(Obstruction::from_word(Obstruction::NONE), None);

    // And a word no build wrote reads as the same thing rather than as the first obstruction.
    assert_eq!(Obstruction::from_word(9), None);
    assert_eq!(Obstruction::from_word(u32::MAX), None);
}

/// An age is written in the two largest units that are not zero, in words, with an `s` on the ones
/// that need it. Two units because this goes at the end of a line that already has a date on it, and
/// a third is a precision nobody acts on.
#[test]
fn an_age_is_written_in_at_most_two_units() {
    let expected = [
        (0, "0 seconds"),
        (1, "1 second"),
        (2, "2 seconds"),
        (45, "45 seconds"),
        (60, "1 minute"),
        (119, "1 minute 59 seconds"),
        (3 * 60, "3 minutes"),
        (60 * 60 - 1, "59 minutes 59 seconds"),
        (60 * 60, "1 hour"),
        (60 * 60 + 5 * 60, "1 hour 5 minutes"),
        (60 * 60 + 1, "1 hour 1 second"),
        (2 * 24 * 60 * 60, "2 days"),
        (3 * 24 * 60 * 60 + 4 * 60 * 60, "3 days 4 hours"),
        // The third unit is dropped rather than shown: two days, an hour and 59 minutes is "2 days
        // 1 hour", not a number of minutes.
        (2 * 24 * 60 * 60 + 60 * 60 + 59 * 60, "2 days 1 hour"),
        // The largest count there is, which in a debug build is where an unchecked addition would
        // panic rather than wrap. The number of days is absurd; the point is that it gets as far as
        // the formatting.
        (u64::MAX, "213503982334601 days 7 hours"),
    ];

    for (secs, written) in expected {
        assert_eq!(render(&age(secs)), written, "an age of {secs} seconds");
    }
}
