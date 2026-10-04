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

use poc_report::{Clock, Leap, Refusal, SNTP_LEN, sntp_reply, sntp_request};

/// The nonce the tests send. Any value works; one that is easy to read in a failure is the point.
const NONCE: u32 = 0x0BAD_F00D;

/// Seconds between the NTP epoch and the Unix one, restated because every timestamp below is built
/// by adding it to a date, and the whole of the 2036 argument is about this number.
const NTP_TO_UNIX: u64 = 2_208_988_800;

/// Seconds since the Unix epoch for a date and a time, written the slow way.
///
/// The arithmetic under test is `poc_report`'s; this is here so that the dates in these tests are
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

/// A well-formed reply: version 4, mode 4, stratum 2, no leap second, the nonce echoed, and a
/// timestamp of the moment given.
///
/// Every test that is not about one of those fields starts from this and changes one thing, so that
/// a failure names the field rather than the packet.
fn reply(epoch_secs: u64) -> Vec<u8> {
    let request = sntp_request(NONCE);
    let mut packet = vec![0; SNTP_LEN];

    // Mode 4 instead of 3: a server, not a client.
    packet[0] = 4 << 3 | 4;
    packet[1] = 2;

    // What a server does with a request: the client's timestamp is copied into the originate field
    // and its own goes in the transmit field.
    packet[24..32].copy_from_slice(&request[40..48]);

    let since_1900 = (epoch_secs + NTP_TO_UNIX).to_be_bytes();
    packet[40..48].copy_from_slice(&since_1900);

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
    assert_eq!(answer.leap, Leap::Normal);
}

/// The time is a count of seconds since 1970, and this is the whole reason for reading eight bytes
/// rather than four: NTP counts from 1900 in a 136-year era, and a 32-bit reading lands in 2036
/// while a 64-bit one does not care. The two cases are 2036 and 2037 because the rollover is
/// between them.
#[test]
fn a_time_past_the_2036_rollover_is_still_a_time() {
    for year in [2035, 2036, 2037, 2100] {
        let moment = epoch_secs(year, 6, 15, 12, 0, 0);

        let answer = sntp_reply(&reply(moment), NONCE).expect("a well-formed reply");

        assert_eq!(
            answer.epoch_secs, moment,
            "the year {} was not read back",
            year
        );
    }
}

/// The timestamp is 64 bits and the era is in the top half, so a reading that only looked at the
/// bottom four would be a date in the far past rather than an error. Moving the era by hand is what
/// makes that case reachable: the seconds within the era are unchanged here and only the top half
/// moves, which is the difference between a date in 2030 and the same date 136 years later.
#[test]
fn a_timestamp_in_a_later_era_keeps_its_era() {
    // A date whose seconds still fit in 32 bits, so that the packet below starts in era zero and
    // the era has to be put there on purpose.
    let moment = epoch_secs(2030, 6, 15, 12, 0, 0);
    assert!(
        moment + NTP_TO_UNIX < 1 << 32,
        "the test date is not in the first era"
    );

    let mut packet = reply(moment);
    packet[40..44].copy_from_slice(&1u32.to_be_bytes());

    let answer = sntp_reply(&packet, NONCE).expect("a well-formed reply");

    assert_eq!(
        answer.epoch_secs,
        moment + (1 << 32),
        "the era was dropped instead of added"
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

/// The other two leap indicators are read rather than refused: a leap second is a fact about tonight
/// and not a reason to distrust the packet.
#[test]
fn a_pending_leap_second_is_reported_rather_than_refused() {
    for (bits, leap) in [(0b0100_0000, Leap::Inserted), (0b1000_0000, Leap::Deleted)] {
        let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
        packet[0] |= bits;

        let answer = sntp_reply(&packet, NONCE).expect("a leap second is not a refusal");

        assert_eq!(answer.leap, leap);
    }

    assert_eq!(
        sentence(Leap::Inserted),
        "a leap second is being added at the end of the day"
    );
    assert_eq!(
        sentence(Leap::Deleted),
        "a leap second is being removed at the end of the day"
    );
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
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[40..48].copy_from_slice(&[0xFF; 8]);

    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::NoTime));
}

/// A server whose clock never worked reports a time before 1970, and there is no epoch to count it
/// from. Rendering one anyway would produce a date in 1899 that looks like a reading.
#[test]
fn a_time_before_the_epoch_is_refused() {
    // 1900-01-01 itself, the NTP epoch: one second before it there is nothing at all, so the first
    // count of seconds is the one that cannot be subtracted.
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[40..48].copy_from_slice(&1u64.to_be_bytes());

    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::BeforeTheEpoch));

    // One second later there is still nothing before the epoch, and a second after that there is.
    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[40..48].copy_from_slice(&(NTP_TO_UNIX - 1).to_be_bytes());
    assert_eq!(sntp_reply(&packet, NONCE), Err(Refusal::BeforeTheEpoch));

    let mut packet = reply(epoch_secs(2026, 10, 4, 18, 22, 31));
    packet[40..48].copy_from_slice(&NTP_TO_UNIX.to_be_bytes());
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

/// The whole point of the time: the seconds a server sends are the seconds the greeting prints. The
/// two are in different crates and one is what the other is for.
#[test]
fn a_time_from_a_server_is_what_the_clock_prints() {
    let moment = epoch_secs(2026, 10, 4, 18, 22, 31);

    let answer = sntp_reply(&reply(moment), NONCE).expect("a well-formed reply");

    assert_eq!(
        poc_report::Clock::utc(answer.epoch_secs).to_string(),
        "2026-10-04 18:22:31"
    );
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

/// The clock and the reply agree at the boundary the greeting prints most often: midnight, where a
/// day of seconds turns into a date that is one further on.
#[test]
fn a_time_at_midnight_moves_the_date() {
    let just_before = epoch_secs(2026, 10, 4, 23, 59, 59);
    let just_after = just_before + 1;

    for (moment, expected) in [
        (just_before, "2026-10-04 23:59:59"),
        (just_after, "2026-10-05 00:00:00"),
    ] {
        let answer = sntp_reply(&reply(moment), NONCE).expect("a well-formed reply");

        assert_eq!(Clock::utc(answer.epoch_secs).to_string(), expected);
    }
}
