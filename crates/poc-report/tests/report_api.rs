// The tests for what this firmware sends to the events API.
//
// They are here in `tests/` for the reason `tests/report.rs` opens with: this crate is `#![no_std]`,
// and an integration test is a separate crate with the standard prelude, so `assert_eq!` and `String`
// are available without the library giving up `no_std` for its own build.
//
// What is worth testing is the body. The body is JSON written by hand, and a body that is wrong is
// wrong in a way nothing on the board can see: a missing brace is a 400 from a server, and
// a timestamp in the wrong format is stored as a string nobody can sort. What an answer means is
// deliberately not tested: a status code is reported as the number it is, because a mapping from
// numbers to sentences goes stale the day a status changes what it means.
//
// **The HTTP framing is not tested here because this crate no longer does any.** It used to: this
// file held the request head, a `status_line_arrived` predicate, and a parser for the status line, and
// the read-boundary bug that shipped — reading a reply once and judging whatever arrived — was caught
// by a test right here. All of that is `edge-http`'s now, and `src/report.rs` builds a
// `RequestHeaders`, writes it, and reads back a `ResponseHeaders` and a `Body`. What is logged there
// is a status code and a body: a number, these bytes, or the fact that neither came.

use std::fmt::Write as _;

use poc_report::{
    Address, Clock, EVENT_TELEMETRY, Event, JoinFailure, LOGGED_LEN, Link, Obstruction,
    REPORT_EVERY_SECS, Reason, Refusal, STALE_AFTER_SECS, Status, Time, Timestamp, logged,
};

/// 2026-10-04T18:22:31Z, written as the arithmetic so the number is not produced by the code under
/// test.
const AT: u64 = 20_730 * 24 * 60 * 60 + 18 * 3_600 + 22 * 60 + 31;

/// The body of a real 401 from this API, captured on 2026-10-07.
///
/// Taken out of the reply it arrived in: the head is [`edge-http`]'s to write and parse, so this
/// file has no opinion about CRLF, about a status line arriving in pieces, or about where the blank
/// line is. What is left is these bytes, the API's own wording rather than a fixture's.
const BODY_401: &[u8] = br#"{"error":"Unauthorized"}"#;

/// The status line of an event from a chip that has joined and has a time.
fn status() -> Status {
    Status {
        time: Time::answered(AT, 2, 5),
        link: Link::Joined,
        address: Some(Address {
            ip: "192.168.0.225".parse().unwrap(),
            prefix_len: 24,
        }),
    }
}

/// Renders a value the way the firmware's `Display2Format` does on the chip, and as `String` here.
fn render(value: &impl std::fmt::Display) -> String {
    let mut written = String::new();

    write!(written, "{value}").expect("writing to a String cannot fail");

    written
}

/// The whole body, for a chip in the state above.
fn body() -> String {
    render(&Event {
        device_id: "esp32c3-001122334455",
        timestamp_secs: AT,
        event_type: EVENT_TELEMETRY,
        status: &status(),
    })
}

/// The body is valid JSON with the four fields the API requires, in its order.
///
/// Pinned as a whole string rather than field by field, because what the API parses is the whole
/// thing: a body with a comma in the wrong place is a 400, and four correct assertions do not notice
/// a fifth one that is not there.
#[test]
fn an_event_is_the_json_the_api_asks_for() {
    assert_eq!(
        body(),
        r#"{"device_id":"esp32c3-001122334455","timestamp":"2026-10-04T18:22:31Z","event_type":"telemetry","payload":"2026-10-04 18:22:31 UTC (from a stratum 2 server), 192.168.0.225/24, wifi: joined"}"#,
    );
}

/// The four fields are all present and all strings, because the API rejects the whole body with a
/// 400 if any of them is missing or is not a string. A `null` for the payload is a 400 too, so the
/// payload is written as a string rather than left as a hole.
#[test]
fn every_field_of_an_event_is_a_json_string() {
    let body = body();

    for field in ["device_id", "timestamp", "event_type", "payload"] {
        assert!(
            body.contains(&format!("\"{field}\":\"")),
            "{field} is missing or is not a string: {body}"
        );
    }

    assert!(body.starts_with('{') && body.ends_with('}'), "{body}");
}

/// The timestamp is what the API will read back as a date, so it has to be RFC 3339 and not the
/// space-separated form the state line prints. The two renderings come out of the same number, and
/// which one goes into the body is the decision this test pins.
#[test]
fn the_timestamp_is_rfc_3339_and_not_the_state_line_s_form() {
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

/// 2028 is a leap year and 2100 is not, and a timestamp that gets February wrong is a body the API
/// stores happily and a reader cannot query.
#[test]
fn the_timestamp_handles_the_ends_of_a_leap_year() {
    // 2028-02-29T12:00:00Z: a leap day in a year divisible by four.
    assert_eq!(
        render(&Timestamp::at(1_835_438_400)),
        "2028-02-29T12:00:00Z",
    );

    // 2100-02-28T23:59:59Z, the last second before a century that is divisible by 100 and not by
    // 400 takes its leap day away. A calendar that divides by four here is wrong once every hundred
    // years, which is exactly the sort of thing that is right in every test somebody writes.
    assert_eq!(
        render(&Timestamp::at(4_107_542_399)),
        "2100-02-28T23:59:59Z"
    );
}

/// Every sentence this crate can put in a body stays quotable without escaping: no `"`, no `\`,
/// nothing below `U+0020`.
///
/// `Quoted` writes a `'"'` and the sentence and another `'"'`, so a sentence that grew one of
/// those would end the JSON string early and turn the rest of the body into something the server
/// either rejects or, worse, parses as a different document. This is the one test that keeps that
/// from happening: a wording that needs quoting fails here rather than in somebody's database.
///
/// One table rather than one test per sentence, because the property is that the set is covered: a
/// sentence added to the firmware without staying inside the alphabet fails to compile, which is
/// the point. The id and the event type are not here — a hex string and a constant, quotable by
/// construction at the call site — and neither is the timestamp, which is digits and fixed
/// punctuation. If the payload ever grows a field from outside (an SSID, a driver's words), this
/// test is where it lands, or the escaping comes back.
#[test]
fn every_sentence_in_a_body_stays_quotable() {
    let mut sentences = vec![
        render(&Address {
            ip: "192.168.0.225".parse().unwrap(),
            prefix_len: 24,
        }),
        render(&Address {
            ip: "10.0.0.7".parse().unwrap(),
            prefix_len: 0,
        }),
        render(&Reason::NoSuchNetwork),
        render(&Reason::SecurityRefused),
        render(&Reason::NoAnswer),
        render(&Reason::HandshakeStalled),
        render(&Reason::LinkLost),
        render(&Reason::Other),
        render(&JoinFailure {
            reason: Reason::NoAnswer,
            signal: Some(-81),
        }),
        render(&JoinFailure {
            reason: Reason::NoSuchNetwork,
            signal: None,
        }),
        render(&Link::NoNetwork),
        render(&Link::UnusableCredential),
        render(&Link::NoRadio),
        render(&Link::Joining),
        render(&Link::Joined),
        render(&Link::Failed(JoinFailure {
            reason: Reason::HandshakeStalled,
            signal: Some(-55),
        })),
        render(&Clock::since_boot(0)),
        render(&Clock::since_boot(u64::MAX)),
        render(&Clock::utc(0)),
        render(&Clock::utc(AT)),
        render(&Clock::utc(u64::MAX)),
        render(&Timestamp::at(0)),
        render(&Timestamp::at(AT)),
        render(&Timestamp::at(u64::MAX)),
        render(&Time::since_boot(63)),
        render(&Time::since_boot_after(63, Obstruction::AnswerTimedOut)),
        render(&Time::since_boot_after(
            63,
            Obstruction::Refused(Refusal::KissOfDeath),
        )),
        render(&Time::answered(AT, 2, 5)),
        render(&Time::answered(AT, 3, STALE_AFTER_SECS + 5 * 60)),
        render(&status()),
        render(&Status {
            time: Time::since_boot_after(63, Obstruction::AnswerTimedOut),
            link: Link::Failed(JoinFailure {
                reason: Reason::NoSuchNetwork,
                signal: Some(-81),
            }),
            address: None,
        }),
    ];

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

    let obstructions = [
        Obstruction::LookupTimedOut,
        Obstruction::RequestTimedOut,
        Obstruction::AnswerTimedOut,
        Obstruction::NoServer,
        Obstruction::Stranger,
        Obstruction::WouldNotSend,
        Obstruction::TooLong,
    ];

    sentences.extend(refusals.iter().map(render));
    sentences.extend(obstructions.iter().map(render));
    sentences.extend(
        refusals
            .iter()
            .map(|refusal| render(&Obstruction::Refused(*refusal))),
    );

    for sentence in &sentences {
        assert!(
            !sentence.chars().any(|c| c == '"' || c == '\\' || c < ' '),
            "a sentence that would end its JSON string: {sentence:?}",
        );
    }
}

/// Two events from the same chip differ in the timestamp and nothing else, and the payload is the
/// state line rather than a second rendering of its three fields.
#[test]
fn the_payload_is_the_state_line() {
    let earlier = render(&Event {
        device_id: "esp32c3-001122334455",
        timestamp_secs: AT - 300,
        event_type: EVENT_TELEMETRY,
        status: &status(),
    });
    let later = body();

    fn payload(body: &str) -> &str {
        body.split(r#""payload":""#)
            .nth(1)
            .expect("a body with a payload")
            .split('"')
            .next()
            .expect("a payload with an end")
    }

    assert_eq!(payload(&earlier), payload(&later));
    assert_eq!(
        payload(&later),
        "2026-10-04 18:22:31 UTC (from a stratum 2 server), 192.168.0.225/24, wifi: joined"
    );
    assert_ne!(
        earlier, later,
        "two events five minutes apart are the same body"
    );
}

/// The body is the API's own words, and on a 400 it is the only thing that says *which* field was
/// wrong. This is the body of the real 401 captured on 2026-10-07, so it is the API's wording rather
/// than a fixture's — and it is logged as-is, because a mapping from numbers to sentences would go
/// stale the day a status changes what it means.
#[test]
fn the_body_is_the_apis_own_words() {
    assert_eq!(render(&logged(BODY_401)), r#"{"error":"Unauthorized"}"#);
}

/// A reply with no body logs as saying so, rather than as an empty line. By the time this crate is
/// handed one, `edge-http` has already said where the body ended, so there is nothing here to guess
/// at: an empty slice is a body of no bytes.
#[test]
fn a_reply_with_no_body_logs_as_saying_so() {
    assert_eq!(render(&logged(b"")), "(no body)");
}

/// A body that is not text is counted rather than rendered: the bytes cannot go into a serial log
/// as they are, and there is no lossy conversion worth having for a line a person reads once.
///
/// The body below mixes printable text with a NUL, a bell, an escape sequence and two bytes no
/// UTF-8 text holds. What the log gets is the count and nothing else, so no byte in it can garble
/// the terminal of whoever is reading.
#[test]
fn a_body_that_is_not_text_is_counted_rather_than_rendered() {
    let body = b"{\"n\":0}\x00\x07\x1b[31m\xff\xfe end";

    assert_eq!(render(&logged(body)), "(20 non-utf8 bytes)");
}

/// Every byte value is safe to hand over, because nothing is rendered before the UTF-8 check: a
/// body from the wire has no type, so there is no value a caller could have promised anything
/// about. All 256 of them together are not UTF-8, so the sweep's own tail is not in the output.
#[test]
fn every_byte_value_can_be_logged() {
    let all: Vec<u8> = (0..=255).collect();

    assert_eq!(render(&logged(&all)), "(256 non-utf8 bytes)");
}

/// A body cut mid-character is cut at the character before it rather than in the middle of its
/// bytes: 119 `a` followed by a two-byte `é` is 121 bytes, so the bound of 120 falls inside the
/// second byte of the `é` and the line holds the 119 `a` plus the mark — never half a character.
#[test]
fn a_body_cut_inside_a_character_is_cut_before_it() {
    let body = "a".repeat(119) + "é";

    let line = render(&logged(body.as_bytes()));

    assert_eq!(line, format!("{}…", "a".repeat(119)));
}

/// The body is cut at a bound and marked when it is, so that a truncated body cannot read as a
/// complete one. A Cloudflare error page is the case this is for: its first 120 characters already
/// say it is HTML, and the other thousand are noise on a line read twice a second.
#[test]
fn a_body_longer_than_the_bound_is_cut_and_says_so() {
    let long = vec![b'x'; LOGGED_LEN + 500];

    let line = render(&logged(&long));

    assert_eq!(line.len(), LOGGED_LEN + '…'.len_utf8());
    assert!(line.ends_with('…'), "{line}");
    assert_eq!(line.chars().take(LOGGED_LEN).count(), LOGGED_LEN);

    // Exactly at the bound is not cut: the ellipsis means "there was more", and adding one when there
    // was not would make every short body look like a fragment of a longer one.
    let exact = vec![b'x'; LOGGED_LEN];
    assert_eq!(render(&logged(&exact)), "x".repeat(LOGGED_LEN));
}

/// Control characters in an otherwise printable body are blanked rather than written through: a
/// newline would split the log line it is printed on, and an escape would reach the reader's
/// terminal. Bodies from this API are JSON without either, so this is a guard rather than
/// a rendering.
#[test]
fn control_characters_in_a_body_are_blanked() {
    assert_eq!(render(&logged(b"a\nb\tc\x1bd")), "a b c d");
}

/// Printable ASCII comes through as itself, which is the other half: replacing everything unprintable
/// with a dot is only safe if what was printable in the first place survives.
#[test]
fn printable_ascii_survives_logging() {
    let printable: Vec<u8> = (0x20..=0x7e).collect();

    assert_eq!(
        render(&logged(&printable)),
        (0x20u8..=0x7e).map(char::from).collect::<String>()
    );
    assert!(
        render(&logged(&printable)).contains('~'),
        "0x7e did not survive"
    );
}

/// Five minutes, and it is a constant here rather than a number in the task that waits so that the
/// interval is one thing the tests can read and the firmware and this test cannot disagree about.
#[test]
fn the_reporting_interval_is_five_minutes() {
    assert_eq!(REPORT_EVERY_SECS, 300);
}

/// The body is a little over two hundred bytes for a chip that has joined, and a long address plus a
/// sentence about a failed join is the largest it gets. A buffer the firmware sizes has to hold the
/// worst case rather than this one, and this is the measurement of the ordinary case that says how
/// much headroom there is.
#[test]
fn a_body_is_small_enough_to_fit_a_buffer() {
    let joined = body().len();

    let failing = render(&Event {
        device_id: "esp32c3-001122334455",
        timestamp_secs: AT,
        event_type: EVENT_TELEMETRY,
        status: &Status {
            time: Time::since_boot_after(3_600, poc_report::Obstruction::AnswerTimedOut),
            link: Link::Failed(poc_report::JoinFailure {
                reason: poc_report::Reason::HandshakeStalled,
                signal: Some(-55),
            }),
            address: Some(Address {
                ip: "192.168.100.200".parse().unwrap(),
                prefix_len: 24,
            }),
        },
    });

    assert!(joined < 256, "{joined} bytes");
    assert!(failing.len() < 512, "{} bytes", failing.len());
    assert!(
        failing.len() > joined,
        "the failing state line is the longer one and this does not see it",
    );
}
