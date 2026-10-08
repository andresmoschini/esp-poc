// The tests for what this firmware sends to the events API, and for what an answer from it means.
//
// They are here in `tests/` for the reason `tests/report.rs` opens with: this crate is `#![no_std]`,
// and an integration test is a separate crate with the standard prelude, so `assert_eq!` and `String`
// are available without the library giving up `no_std` for its own build.
//
// What is worth testing is the body and what an answer means. The body is JSON written by hand, and a
// body that is wrong is wrong in a way nothing on the board can see: a missing brace is a 400 from a
// server, an unescaped quote is a 400, and a timestamp in the wrong format is stored as a string
// nobody can sort. An answer off the network is untrusted input, and what to say about it is what
// ends up in a serial log.
//
// **The HTTP framing is not tested here because this crate no longer does any.** It used to: this
// file held the request head, a `status_line_arrived` predicate, and a parser for the status line, and
// the read-boundary bug that shipped — reading a reply once and judging whatever arrived — was caught
// by a test right here. All of that is `edge-http`'s now, and `src/report.rs` builds a
// `RequestHeaders`, writes it, and reads back a `ResponseHeaders` and a `Body`. What arrives here is a
// status number, a body, or the fact that neither came: three constructors rather than one parse. So
// the tests below are about the wording, which is what a reader of the log actually sees, and about
// the mapping from a number to a sentence — the parts a general-purpose client cannot decide.

use std::fmt::Write as _;

use poc_report::{
    Address, EVENT_TELEMETRY, Event, LOGGED_LEN, Link, REPORT_EVERY_SECS, Reply, Status, Time,
    Timestamp, Verdict, logged,
};

/// 2026-10-04T18:22:31Z, written as the arithmetic so the number is not produced by the code under
/// test.
const AT: u64 = 20_730 * 24 * 60 * 60 + 18 * 3_600 + 22 * 60 + 31;

/// The body of a real 401 from this API, captured on 2026-10-07.
///
/// Taken out of the reply it arrived in: the head is [`edge-http`]'s to write and parse now, so this
/// file has no opinion about CRLF, about a status line arriving in pieces, or about where the blank
/// line is. What is left is what this crate is given — a number, these bytes, or the fact that
/// neither arrived — and that is what the tests below are about.
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

/// A quote in any string field would end the JSON string early and turn the rest of the body into
/// something the server either rejects or, worse, parses as a different document.
///
/// The status line is the field that could grow one without anybody deciding to: it is a sentence
/// this crate writes, and a sentence about a radio is exactly the kind of thing that eventually
/// quotes the firmware. A device id comes from outside in principle and an event type from a
/// constant, so all three are exercised here.
#[test]
fn a_quote_in_a_field_is_escaped_rather_than_ending_the_string() {
    let body = render(&Event {
        device_id: r#"esp"32c3"#,
        timestamp_secs: AT,
        event_type: EVENT_TELEMETRY,
        status: &status(),
    });

    // The quote is still in the body — as part of the device id — but escaped, so the document has
    // one string where it had one before.
    assert!(body.starts_with(r#"{"device_id":"esp\"32c3","#), "{body}");
    // One escaped quote, and the four fields are still four strings: a quote that ended the string
    // early would have swallowed the rest of the document into one field.
    assert_eq!(body.matches(r#"\""#).count(), 1, "{body}");
    // All four fields are still there, in order: a quote that ended the string early would have
    // swallowed the rest of the document into the device id.
    let mut rest = body.as_str();

    for field in ["device_id", "timestamp", "event_type", "payload"] {
        let found = rest
            .find(field)
            .unwrap_or_else(|| panic!("{field} is gone: {body}"));

        rest = &rest[found + field.len()..];
    }
}

/// A backslash is the other half of the same rule: JSON spells `\"` as an escape, so a literal
/// backslash has to become `\\` or the character after it is swallowed as part of an escape.
#[test]
fn a_backslash_in_a_field_is_escaped() {
    let body = render(&Event {
        device_id: r"domain\chip",
        timestamp_secs: AT,
        event_type: EVENT_TELEMETRY,
        status: &status(),
    });

    assert!(
        body.starts_with(r#"{"device_id":"domain\\chip","#),
        "{body}"
    );
}

/// JSON forbids raw control characters in a string. The one that can appear here without anybody
/// trying is a newline, and a status line is the sort of thing that grows one.
#[test]
fn a_control_character_in_a_field_is_escaped() {
    let body = render(&Event {
        device_id: "two\nlines",
        timestamp_secs: AT,
        event_type: EVENT_TELEMETRY,
        status: &status(),
    });

    assert!(body.starts_with(r#"{"device_id":"two\nlines","#), "{body}");
    assert!(!body.contains('\n'), "{body}");
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

/// Nothing at all is its own answer, and it is not the same thing as a portal's login page: one means
/// nothing answered and the other means something that is not the API answered. They want different
/// fixes, so they do not share a sentence.
///
/// Both are **constructed** rather than parsed, because `edge-http` decides which of the two happened
/// and hands over the fact rather than the bytes. That is the trade this crate made: the decision of
/// where a reply ends moves to a library, and what stays here is the wording — which is what the gate
/// can check and what a log actually shows.
#[test]
fn no_bytes_at_all_is_not_the_same_as_something_that_is_not_the_api() {
    assert_eq!(
        Reply::nothing_came_back().verdict(),
        Verdict::NothingCameBack
    );
    assert_eq!(
        render(&Verdict::NothingCameBack),
        "nothing came back: the connection closed silently",
    );

    assert_eq!(Reply::not_the_api().verdict(), Verdict::NotTheApi);
    assert_eq!(
        render(&Verdict::NotTheApi),
        "what answered was not the API: the first line was not a status line",
    );
}

/// Both ways of not having an answer report no status, rather than a zero. Inventing a number would be
/// a guess: a zero in a log reads as a status the API sent, and there is no such status. `None` is
/// the truth and the caller says so.
#[test]
fn a_reply_without_a_status_has_no_status_code() {
    for reply in [Reply::nothing_came_back(), Reply::not_the_api()] {
        assert_eq!(reply.status(), None, "{reply:?}");
    }
}

/// The status code is the one thing in a reply that is not this firmware's opinion, so it has to be
/// readable for every status and not only for the ones this crate happens to have a name for. That
/// was the actual gap: a verdict on its own throws 401 away and keeps 503, which is backwards.
#[test]
fn the_status_code_is_readable_for_every_status() {
    for status in [200u16, 201, 204, 400, 401, 403, 404, 405, 429, 500, 503] {
        assert_eq!(
            Reply::answered(status, b"").status(),
            Some(status),
            "{status}",
        );
    }
}

/// The body is the API's own words, and on a 400 it is the only thing that says *which* field was
/// wrong. This is the body of the real 401 captured on 2026-10-07, so it is the API's wording rather
/// than a fixture's.
#[test]
fn the_body_is_the_apis_own_words() {
    let reply = Reply::answered(401, BODY_401);

    assert_eq!(reply.body(), BODY_401);
    assert_eq!(render(&logged(reply.body())), r#"{"error":"Unauthorized"}"#);
}

/// A reply with no body logs as saying so, rather than as an empty line. By the time this crate is
/// handed one, `edge-http` has already said where the body ended, so there is nothing here to guess
/// at: an empty slice is a body of no bytes. The reason is in [`logged`] — the body is the only thing
/// the status line does not already say.
#[test]
fn a_reply_with_no_body_logs_as_saying_so() {
    assert_eq!(Reply::answered(204, b"").body(), b"");
    assert_eq!(render(&logged(b"")), "(no body)");
}

/// A body is untrusted bytes and it goes into a serial log, so anything unprintable is replaced
/// rather than written through: a NUL or an escape sequence in a reply would garble the terminal of
/// whoever is reading, which destroys the output the line exists to produce.
#[test]
fn a_body_that_is_not_text_cannot_break_the_log() {
    let body = b"{\"n\":0}\x00\x07\x1b[31m\xff\xfe end";

    let line = render(&logged(body));

    // The escape is replaced and the `[31m` after it is not: what follows an escape sequence is
    // ordinary printable text once the escape that introduced it is gone, and there is nothing left
    // for a terminal to interpret. What matters is that the `0x1b` itself did not reach the log.
    assert_eq!(line, "{\"n\":0}···[31m·· end");
    assert!(
        !line.chars().any(|character| character.is_control()),
        "a control character reached the log: {line:?}",
    );
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

/// Every byte value survives `logged` without becoming a control character or a panic. This is the
/// property that makes it safe to hand it anything off the network, and a sweep is the only way to
/// check all 256 of them — a body from the wire has no type, so there is no value a caller could have
/// promised anything about.
#[test]
fn every_byte_value_can_be_logged() {
    let all: Vec<u8> = (0..=255).collect();

    let line = render(&logged(&all));

    assert!(
        !line.chars().any(|character| character.is_control()),
        "a control character reached the log: {line:?}",
    );
    // Cut at the bound, so the sweep's own tail is not in the output; every byte that was written
    // came out as one character and none of them was a control character.
    assert_eq!(line.chars().count(), LOGGED_LEN + 1, "the ellipsis");
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

/// The 201 the API answers a stored event with is the one that means the exchange worked, and it is
/// the only answer that does.
#[test]
fn a_stored_event_is_recognized() {
    assert_eq!(Reply::answered(201, b"").verdict(), Verdict::Stored,);
    assert_eq!(render(&Verdict::Stored), "the API stored the event");
}

/// The 401 this firmware gets today is the point of the exercise: the request went out, the API read
/// it, and it was refused for want of credentials. That is three facts and the status line carries
/// all three.
#[test]
fn a_refusal_for_want_of_credentials_is_recognized() {
    assert_eq!(
        Reply::answered(401, BODY_401).verdict(),
        Verdict::Unauthorized,
    );
    assert_eq!(
        render(&Verdict::Unauthorized),
        "the API refused the event: no credentials were sent with it",
    );
}

/// Every other status is named as a number rather than guessed at, because a status this firmware
/// does not know is a status somebody has to go and read about.
#[test]
fn an_unnamed_status_is_reported_as_the_number_it_is() {
    for status in [200u16, 204, 301, 404, 429, 500, 503] {
        assert_eq!(
            Reply::answered(status, b"").verdict(),
            Verdict::Unexpected(status),
            "{status}",
        );
    }

    assert_eq!(
        render(&Verdict::Unexpected(503)),
        "the API answered with a status this firmware does not name: 503",
    );
}

/// Every verdict has a sentence, because the sentence is what ends up in the serial log, and a log
/// line saying `Unauthorized` answers "what happened" and not "what now".
#[test]
fn every_verdict_says_something() {
    let expected = [
        (Verdict::Stored, "the API stored the event"),
        (
            Verdict::Unauthorized,
            "the API refused the event: no credentials were sent with it",
        ),
        (
            Verdict::Malformed,
            "the API could not read the event: it rejected the body",
        ),
        (
            Verdict::NotAllowed,
            "the API would not take a POST on this path",
        ),
        (
            Verdict::Unexpected(500),
            "the API answered with a status this firmware does not name: 500",
        ),
        (
            Verdict::NotTheApi,
            "what answered was not the API: the first line was not a status line",
        ),
        (
            Verdict::NothingCameBack,
            "nothing came back: the connection closed silently",
        ),
    ];

    for (verdict, words) in expected {
        assert_eq!(render(&verdict), words, "sentence for {verdict:?}");
    }
}

/// Two verdicts that read alike are two that will be confused, and the set here has one case per
/// thing the API can do with this exchange: take it, refuse it for want of credentials, fail to
/// parse it, refuse the method, say something else, answer with something that is not the API, or
/// not answer at all.
#[test]
fn the_answers_are_distinguishable() {
    let sentences = [
        Verdict::Stored,
        Verdict::Unauthorized,
        Verdict::Malformed,
        Verdict::NotAllowed,
        Verdict::Unexpected(500),
        Verdict::NotTheApi,
        Verdict::NothingCameBack,
    ]
    .map(|verdict| render(&verdict));

    let mut unique = sentences.to_vec();
    unique.sort_unstable();
    unique.dedup();

    assert_eq!(unique.len(), sentences.len(), "two answers read the same");
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
