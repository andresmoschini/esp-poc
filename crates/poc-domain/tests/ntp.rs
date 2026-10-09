// The tests for reading an SNTP packet.
//
// They are here, in `tests/`, for the reason `tests/report.rs` opens with: this crate is
// `#![no_std]`, and an integration test is a separate crate with the standard prelude, so the
// assertions can be written without the library giving up `no_std` for its own build.
//
// What is worth testing is everything a server could send that this firmware would rather it did
// not: a packet that is not an answer, a stratum that means the server is not answering, a timestamp
// from a server whose clock never worked. A packet off the network is untrusted input, and none of
// it can be checked on a board without a server to send it.
//
// One rule for every test in this file, learned the hard way: build the packet from the *fields* on
// the wire, never from the numbers the code under test produces. The first version of `reply` below
// wrote the timestamp with the same misunderstanding the reader had — all eight bytes as seconds — so
// twenty tests agreed with a bug that a real server's first reply exposed. An encoder and a decoder
// written from the same idea check each other's arithmetic and neither one's idea of the format.

use poc_domain::{Obstruction, Refusal, SNTP_LEN, sntp_reply, sntp_request};

/// The nonce the tests send. Any value works; one that is easy to read in a failure is the point.
const NONCE: u32 = 0x0BAD_F00D;

/// Seconds between the NTP epoch and the Unix one, restated because every timestamp below is built
/// by adding it to a date, and reading one is a subtraction of it.
const NTP_TO_UNIX: u64 = 2_208_988_800;

/// Seconds since the Unix epoch for a date and a time, written the slow way.
///
/// The arithmetic under test is `poc_domain`'s; this is here so that the dates in these tests are
/// not produced by it, which would make every one of them agree with a mistake.
fn epoch_secs(year: u64, month: u64, day: u64, hour: u64, minute: u64, second: u64) -> u64 {
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

    days * 86_400 + hour * 3_600 + minute * 60 + second
}

/// The 8 bytes of an NTP timestamp for a date, as a server would put them on the wire: 32 bits of
/// whole seconds since 1900, then 32 bits of the fraction of a second.
///
/// The fraction is a parameter because that is the whole point — the reader has to drop it, and a
/// helper that always wrote zero would not notice either way. The seconds are the count, and the
/// count is put in the *first* four bytes, which is the mistake this file now exists to catch.
fn timestamp_bytes(epoch_secs: u64, fraction: u32) -> [u8; 8] {
    let seconds = u32::try_from(epoch_secs + NTP_TO_UNIX).expect("a date that era 0 can express");

    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&seconds.to_be_bytes());
    bytes[4..].copy_from_slice(&fraction.to_be_bytes());

    bytes
}

/// A well-formed reply: version 4, mode 4, stratum 2, no leap second, the nonce echoed, and a
/// timestamp of the moment given at a fraction of a second past it.
///
/// Every test that is not about one of those fields starts from this and changes one thing, so that
/// a failure names the field rather than the packet.
fn reply(epoch_secs: u64) -> Vec<u8> {
    reply_at(epoch_secs, 0)
}

/// As [`reply`], at a named fraction of a second.
fn reply_at(epoch_secs: u64, fraction: u32) -> Vec<u8> {
    let request = sntp_request(NONCE);
    let mut packet = vec![0; SNTP_LEN];

    // Mode 4 instead of 3: a server, not a client.
    packet[0] = 4 << 3 | 4;
    packet[1] = 2;

    // What a server does with a request: the client's timestamp is copied into the originate field
    // and its own goes in the transmit field.
    packet[24..32].copy_from_slice(&request[40..48]);
    packet[40..48].copy_from_slice(&timestamp_bytes(epoch_secs, fraction));

    packet
}

/// What the firmware would print for something with a sentence, so that the tests pin the wording
/// and not only the choice.
fn sentence(what: impl std::fmt::Display) -> String {
    use std::fmt::Write as _;

    let mut written = String::new();
    write!(written, "{what}").expect("writing to a String cannot fail");
    written
}

/// A reply that is an answer to this request is a time, and the seconds come out as they went in.
#[test]
fn a_reply_carries_the_time_the_server_gave() {
    let moment = epoch_secs(2026, 10, 4, 18, 22, 31);

    let answer = sntp_reply(&reply(moment), NONCE).expect("a well-formed reply");

    assert_eq!(answer.epoch_secs, moment);
    assert_eq!(answer.stratum, 2);
}

/// The second half of a timestamp is a fraction, and adding it to the seconds is a bug that looks
/// like a server sending nonsense: a real reply of 2026-10-04 12:24:25.292 came back as the year
/// 544426464172, which is `0xEE6CC3F9_4ABFCE7E` read as one number.
///
/// Every fraction from nothing to almost a whole second has to leave the reading untouched, and the
/// ones at the ends are where a decoder that rounds or that shifts by a byte would show it.
#[test]
fn the_fraction_of_a_second_is_not_part_of_the_seconds() {
    let moment = epoch_secs(2026, 10, 4, 12, 24, 25);

    for fraction in [0, 1, 0x4000_0000, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF] {
        let answer = sntp_reply(&reply_at(moment, fraction), NONCE).expect("a well-formed reply");

        assert_eq!(
            answer.epoch_secs, moment,
            "a fraction of {fraction:#010x} moved the time"
        );
    }
}

/// The whole timestamp from a real reply, byte for byte, because a test built from the same idea as
/// the code cannot catch the code being wrong about the idea. These are the eight bytes a server on a
/// home connection sent on 2026-10-04: seconds `0xEE6CC3F9` and a fraction of 0.292.
#[test]
fn a_reply_captured_from_a_real_server_reads_as_the_day_it_was_asked_for() {
    let mut packet = reply(0);

    // 0xEE6CC3F9 seconds since 1900 is 2026-10-04 12:24:25 UTC, and the low half is a fifth of a
    // second rather than 1_254_084_222 seconds.
    packet[40..48].copy_from_slice(&[0xEE, 0x6C, 0xC3, 0xF9, 0x4A, 0xBF, 0xCE, 0x7E]);

    let answer = sntp_reply(&packet, NONCE).expect("a well-formed reply");

    assert_eq!(
        answer.epoch_secs,
        epoch_secs(2026, 10, 4, 12, 24, 25),
        "a packet from a real server read as something else",
    );
}

/// Era 0 ends on 2036-02-07, and the seconds field uses all 32 bits long before that — a 2026 date is
/// past 2^31 seconds since 1900, so its top bit is set and is not a flag of anything. Both ends of
/// era 0 are pinned here because a decoder that treated the top bit as an era number, or that read
/// five bytes, would be right in the middle of the range and wrong at both ends.
#[test]
fn the_ends_of_the_first_era_are_readable() {
    // The first readable moment of era 0 is 1970-01-01 itself: the seconds field holds the count
    // since 1900, and 2 208 988 800 of them is the Unix epoch. A second earlier is a time before it,
    // which is a refusal rather than a reading.
    let mut packet = reply(0);
    packet[40..44].copy_from_slice(&u32::try_from(NTP_TO_UNIX).unwrap().to_be_bytes());
    packet[44..48].copy_from_slice(&[0; 4]);
    assert_eq!(sntp_reply(&packet, NONCE).map(|a| a.epoch_secs), Ok(0));

    // The last second era 0 can express is all ones, which is also the value this refuses as a
    // server with no clock, so the last readable moment is the second before it.
    let last = epoch_secs(2036, 2, 7, 6, 28, 14);
    assert_eq!(
        sntp_reply(&reply(last), NONCE).map(|a| a.epoch_secs),
        Ok(last),
        "the end of era 0 did not read back",
    );
}

/// A server that does not echo the nonce answered somebody else, or is repeating an answer that was
/// already given. This is the check that stands between an open UDP port and a clock that believes
/// whatever arrives first.
#[test]
fn a_reply_that_echoes_another_nonce_is_refused() {
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[28..32].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());

    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::NotOurs));
    assert_eq!(
        sentence(Refusal::NotOurs),
        "the packet is not an answer to this client's request"
    );
}

/// The client's own request, sent back to it, is the case the echo check exists for and the mode
/// check catches before it: a server that echoed without answering would be a different failure.
#[test]
fn a_request_is_not_a_reply() {
    let request = sntp_request(NONCE);

    assert_eq!(sntp_reply(&request, NONCE), Err(Refusal::NotAReply));
}

/// Stratum 0 is not a server that cannot answer: it is a server explaining why it will not, and
/// treating it as a time would set the clock to 1970.
#[test]
fn a_kiss_of_death_is_refused() {
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[1] = 0;

    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::KissOfDeath));
}

/// A leap indicator of three is the server saying that it does not consider its own clock sound. Its
/// time may still be nearly right, and the point is that "nearly" is not something this firmware can
/// judge from a packet.
#[test]
fn a_server_that_says_it_is_unsynchronized_is_refused() {
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[0] |= 0b1100_0000;

    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::Unsynchronized));
}

/// The other two leap indicators are read rather than refused, and then nothing is done with them: a
/// leap second is a fact about tonight, and a clock whose use for its time is printing it will not
/// notice either a repeated second or a skipped one. The bits are still parsed, because the third
/// value in those same two bits is a refusal and there is no way to tell them apart without looking.
#[test]
fn a_pending_leap_second_is_not_a_refusal() {
    for bits in [0b0100_0000, 0b1000_0000] {
        let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
        packet[0] |= bits;

        assert!(
            sntp_reply(&packet, NONCE).is_ok(),
            "a leap second is not a reason to distrust the packet",
        );
    }
}

/// A packet too short to be a header has nothing in it to read, and reading past the end of it would
/// be a bug on a board that has to answer.
#[test]
fn a_packet_shorter_than_a_header_is_refused() {
    let packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));

    for length in [0, 1, 24, 40, SNTP_LEN - 1] {
        assert_eq!(
            sntp_reply(&packet[..length], NONCE),
            Err(Refusal::Short),
            "a packet of {length} bytes"
        );
    }
}

/// A packet larger than a header is not an error: servers are allowed to send extension fields, and
/// the time is in the first 48 bytes of any of them. This is why the firmware's receive buffer is
/// larger than the header.
#[test]
fn a_packet_longer_than_a_header_is_still_a_time() {
    let moment = epoch_secs(2026, 10, 4, 18, 22, 31);

    let mut packet = reply(moment);
    packet.extend_from_slice(&[0; 32]);

    assert_eq!(
        sntp_reply(&packet, NONCE).expect("extension fields are not a refusal"),
        sntp_reply(&reply(moment), NONCE).expect("a well-formed reply"),
    );
}

/// An all-ones timestamp is how a server writes down that it has no time to give, and reading it as a
/// number would be a date in the year 584 billion.
#[test]
fn a_server_with_no_time_is_refused() {
    // A zero seconds field is what RFC 5905 defines as unknown or unsynchronized time, and an
    // all-ones one is what several implementations send for a clock they have never set. Neither is
    // a reading: the first is 1900, the second the last second era 0 can express.
    for field in [[0; 4], [0xFF; 4]] {
        let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
        packet[40..44].copy_from_slice(&field);
        packet[44..48].copy_from_slice(&[0; 4]);

        assert_eq!(
            sntp_reply(&packet, NONCE),
            Err(Refusal::NoTime),
            "{field:02x?}"
        );
    }
}

/// A server whose clock never worked reports a time before 1970, and there is no epoch to count it
/// from. Rendering one anyway would produce a date in 1899 that looks like a reading.
#[test]
fn a_time_before_the_epoch_is_refused() {
    let ntp_epoch = u32::try_from(NTP_TO_UNIX).unwrap();

    // 1900-01-01 itself, the NTP epoch: there is no time at all before it, so a count of one second
    // is the first that cannot be turned into seconds since 1970.
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[40..44].copy_from_slice(&1u32.to_be_bytes());
    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::BeforeTheEpoch));

    // A second before the Unix epoch is still before it, and a second after is 1970-01-01T00:00:01Z.
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[40..44].copy_from_slice(&(ntp_epoch - 1).to_be_bytes());
    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::BeforeTheEpoch));

    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[40..44].copy_from_slice(&ntp_epoch.to_be_bytes());
    assert_eq!(sntp_reply(&packet, NONCE).map(|a| a.epoch_secs), Ok(0));
}

/// A stratum the protocol has no meaning for is not a time, and the stratum is the one field that is
/// a plain number where a mistake is silent.
#[test]
fn a_stratum_no_protocol_has_is_refused() {
    for stratum in [16, 32, 255] {
        let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
        packet[1] = stratum;

        assert_eq!(
            sntp_reply(&packet, NONCE),
            Err(Refusal::NotAServer),
            "stratum {stratum}"
        );
    }
}

/// A version this client does not speak is refused rather than guessed at: the offsets in the header
/// are version 4's, and reading them as another version's would be arithmetic on nothing.
#[test]
fn another_version_is_refused() {
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[0] = 3 << 3 | 4;

    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::Version));
}

/// The whole point of the time: the seconds a server sends are the seconds the clock counts from.
/// The two are in different crates and one is what the other is for, and what travels between them
/// is the count rather than a rendering of it.
#[test]
fn a_time_from_a_server_is_what_the_clock_counts() {
    let moment = epoch_secs(2026, 10, 4, 18, 22, 31);

    let answer = sntp_reply(&reply(moment), NONCE).expect("a well-formed reply");

    assert_eq!(answer.epoch_secs, moment);
}

/// A request is 48 zero bytes with two things in it, and both are checked here rather than assumed:
/// a client that asked as a server would be ignored, and one that did not put the nonce in would be
/// unable to tell an answer from a stranger.
#[test]
fn a_request_is_a_client_asking_with_a_nonce_in_it() {
    let request = sntp_request(NONCE);

    assert_eq!(request.len(), SNTP_LEN);
    assert_eq!(request[0], 4 << 3 | 3, "version 4, mode 3, no leap warning");
    assert_eq!(
        &request[44..48],
        &NONCE.to_be_bytes(),
        "the nonce is where it is read from"
    );
    assert_eq!(
        request.iter().filter(|byte| **byte != 0).count(),
        5,
        "nothing but the mode and the nonce is set"
    );
}

/// Two different requests differ in the nonce and in nothing else, so the echo check is checking the
/// one field that changes.
#[test]
fn two_requests_differ_only_in_their_nonce() {
    let first = sntp_request(1);
    let second = sntp_request(2);

    for (index, (left, right)) in first.iter().zip(second.iter()).enumerate() {
        if (44..48).contains(&index) {
            continue;
        }
        assert_eq!(left, right, "byte {index} is not the nonce");
    }
}

/// Every refusal has a sentence, because the sentence is what ends up in the log: a serial output
/// full of `Refused(NotOurs)` answers "what happened" and not "what now".
#[test]
fn every_refusal_says_something() {
    let expected = [
        (Refusal::Short, "the packet is shorter than an SNTP header"),
        (Refusal::NotAReply, "the packet is not a reply to a client"),
        (
            Refusal::Version,
            "the packet is a version of SNTP this client does not speak",
        ),
        (
            Refusal::KissOfDeath,
            "the server will not answer this client",
        ),
        (
            Refusal::NotAServer,
            "the packet names a stratum no version of SNTP has",
        ),
        (
            Refusal::Unsynchronized,
            "the server says it is not synchronized to anything",
        ),
        (
            Refusal::NotOurs,
            "the packet is not an answer to this client's request",
        ),
        (Refusal::NoTime, "the server's packet carries no time"),
        (
            Refusal::BeforeTheEpoch,
            "the server's time is before 1970, so there is no epoch to count it from",
        ),
    ];

    for (refusal, words) in expected {
        assert_eq!(sentence(refusal), words, "sentence for {refusal:?}");
    }
}

/// Two refusals that read alike are two that will be confused, and the set here has one case per
/// thing a server can do wrong: a wrong mode, a wrong version, a stratum that means no, a clock that
/// says it is lost, a packet that is not an answer, and a time that cannot exist.
#[test]
fn the_refusals_are_distinguishable() {
    let sentences = [
        Refusal::Short,
        Refusal::NotAReply,
        Refusal::Version,
        Refusal::KissOfDeath,
        Refusal::NotAServer,
        Refusal::Unsynchronized,
        Refusal::NotOurs,
        Refusal::NoTime,
        Refusal::BeforeTheEpoch,
    ]
    .map(sentence);

    let mut unique = sentences.to_vec();
    unique.sort_unstable();
    unique.dedup();

    assert_eq!(unique.len(), sentences.len(), "two refusals read the same");
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

    for (obstruction, words) in expected {
        assert_eq!(sentence(obstruction), words, "sentence for {obstruction:?}");
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
    .map(|obstruction| sentence(obstruction));

    let mut unique = sentences.to_vec();
    unique.sort_unstable();
    unique.dedup();

    assert_eq!(
        unique.len(),
        sentences.len(),
        "two obstructions read the same"
    );
}
