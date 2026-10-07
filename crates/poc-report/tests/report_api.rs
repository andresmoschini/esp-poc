// The tests for what this firmware sends to the events API, and for what an answer from it means.
//
// They are here in `tests/` for the reason `tests/report.rs` opens with: this crate is `#![no_std]`,
// and an integration test is a separate crate with the standard prelude, so `assert_eq!` and `String`
// are available without the library giving up `no_std` for its own build.
//
// What is worth testing is the wire format and the reading of an answer. The body is JSON written by
// hand, and a body that is wrong is wrong in a way nothing on the board can see: a missing brace is
// a 400 from a server, an unescaped quote is a 400, and a timestamp in the wrong format is stored as
// a string nobody can sort. The status line is the other direction — an answer off the network is
// untrusted input, and "the first fifteen bytes were HTML" is the case a chip on a hotel network
// actually hits.

use std::fmt::Write as _;

use poc_report::{
    Address, EVENT_TELEMETRY, EVENTS_PATH, Event, Link, REPORT_EVERY_SECS, Request, Status, Time,
    Timestamp, Verdict, status_line_arrived,
};

/// 2026-10-04T18:22:31Z, written as the arithmetic so the number is not produced by the code under
/// test.
const AT: u64 = 20_730 * 24 * 60 * 60 + 18 * 3_600 + 22 * 60 + 31;

/// A real 401 from this API, captured on 2026-10-07 with the bytes the firmware sends.
///
/// The `Date`, `CF-RAY`, `Report-To` and `Nel` headers are trimmed, and everything else is byte for
/// byte what came back — in particular the `Connection: close` and the 24-byte JSON body, both of
/// which are what a real worker sends and neither of which a hand-written fixture would have thought
/// of. It is 650 bytes against a 25-byte status line, which is the ratio that makes the read-boundary
/// bug in [`Verdict::from_reply`] ordinary rather than exotic: one read of this reply is very likely
/// not the whole of it.
const REPLY_401: &[u8] = b"HTTP/1.1 401 Unauthorized\r\n\
Date: Wed, 07 Oct 2026 11:36:42 GMT\r\n\
Content-Type: application/json\r\n\
Content-Length: 24\r\n\
Connection: close\r\n\
WWW-Authenticate: Bearer realm=\"cfpoc\"\r\n\
Server: cloudflare\r\n\
\r\n\
{\"error\":\"Unauthorized\"}";

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

/// The head is the part a server refuses a request over rather than stores wrongly, and CRLF is the
/// detail: a request whose lines end in a bare `\n` is a request some servers will not answer at
/// all, which on a board looks exactly like the API being down.
#[test]
fn the_head_is_an_http_1_1_post_with_crlf_lines() {
    let head = render(&Request {
        host: "cfpoc.example.workers.dev",
        path: EVENTS_PATH,
        content_length: 42,
    });

    assert_eq!(
        head,
        concat!(
            "POST /events HTTP/1.1\r\n",
            "Host: cfpoc.example.workers.dev\r\n",
            "Content-Type: application/json\r\n",
            "Content-Length: 42\r\n",
            "Connection: close\r\n",
            "\r\n",
        ),
    );
}

/// `Content-Length` is the number the caller measured, and the body is written into a buffer before
/// the head is built because the head cannot be written without it. This is the pairing of the two
/// that matters: a length that does not match the body is a request a server waits on until it gives
/// up, which is a hang rather than an error.
#[test]
fn the_content_length_is_the_length_the_caller_measured() {
    let event = Event {
        device_id: "esp32c3-001122334455",
        timestamp_secs: AT,
        event_type: EVENT_TELEMETRY,
        status: &status(),
    };

    let body = render(&event);

    let head = render(&Request {
        host: "cfpoc.example.workers.dev",
        path: EVENTS_PATH,
        content_length: body.len(),
    });

    assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())));
    assert!(
        head.ends_with("\r\n\r\n"),
        "the head ends where the body begins"
    );
}

/// A reply arrives in as many reads as the network decides, and the status line is not guaranteed to
/// be whole in the first one. Measured against this API, the real 401 below is 650 bytes and the
/// status line is the first 25 of them, so a read that lands inside the line is ordinary rather than
/// exotic.
///
/// This is the bug this file exists to stop repeating. The firmware read the reply exactly once and
/// judged whatever arrived, so a read landing inside the line reported "what answered was not the
/// API" — for a reply the API had already sent, in full, correctly. Both halves are pinned here: the
/// reader must be able to say when the line is whole, and once it says so every prefix must give the
/// same answer.
#[test]
fn a_reply_read_one_byte_at_a_time_is_still_a_reply() {
    let line_end = REPLY_401
        .windows(2)
        .position(|pair| pair == b"\r\n")
        .expect("a status line");

    for read in 1..=line_end {
        let arrived = &REPLY_401[..read];

        // Before the CRLF arrives the reader must know it has to read more, and must not have an
        // opinion about what it has.
        assert!(
            !status_line_arrived(arrived),
            "{read} bytes of a reply were taken for a whole first line: {}",
            String::from_utf8_lossy(arrived),
        );
    }

    // The CRLF itself, which is the byte that ends the line: one byte earlier than this the line is
    // not whole and one byte later it is.
    assert!(!status_line_arrived(&REPLY_401[..line_end + 1]));
    assert!(status_line_arrived(&REPLY_401[..line_end + 2]));

    // From the moment the line is whole, every byte that can be in the buffer after it gives the same
    // answer, including the CRLF, the headers, and the body.
    for read in (line_end + 2)..=REPLY_401.len() {
        assert_eq!(
            Verdict::from_reply(&REPLY_401[..read]),
            Verdict::Unauthorized,
            "{read} bytes of a valid reply read as something else: {}",
            String::from_utf8_lossy(&REPLY_401[..read]),
        );
    }
}

/// The point of the loop above, stated on its own: a first line that has half arrived and a first line
/// that is wrong look identical in the bytes, and the firmware has to be able to tell them apart
/// rather than guess. This is the exact prefix that made the guess, and what it is made of.
#[test]
fn a_half_arrived_status_line_is_not_the_same_as_a_wrong_one() {
    let half = &REPLY_401[..11];

    assert_eq!(half, b"HTTP/1.1 40");
    assert!(
        !status_line_arrived(half),
        "a truncated line was taken for a whole one",
    );
    assert_eq!(
        Verdict::from_reply(b"HTTP/1.1 2011 Created\r\n\r\n"),
        Verdict::NotTheApi,
        "a whole line that is not a status line is still a whole line",
    );
}

/// Nothing at all is its own answer, and it is not the same thing as a portal's login page: one means
/// nothing answered and the other means something that is not the API answered. They want different
/// fixes, so they do not share a sentence.
#[test]
fn no_bytes_at_all_is_not_the_same_as_something_that_is_not_the_api() {
    assert_eq!(Verdict::from_reply(b""), Verdict::NothingCameBack);
    assert_eq!(
        render(&Verdict::NothingCameBack),
        "nothing came back: the connection closed silently",
    );

    assert_eq!(
        Verdict::from_reply(b"<!DOCTYPE html><html>..."),
        Verdict::NotTheApi,
    );
}

/// The 201 the API answers a stored event with is the one that means the exchange worked, and it is
/// the only answer that does.
#[test]
fn a_stored_event_is_recognized() {
    assert_eq!(
        Verdict::from_reply(b"HTTP/1.1 201 Created\r\n\r\n"),
        Verdict::Stored,
    );
    assert_eq!(render(&Verdict::Stored), "the API stored the event");
}

/// The 401 this firmware gets today is the point of the exercise: the request went out, the API read
/// it, and it was refused for want of credentials. That is three facts and the status line carries
/// all three.
#[test]
fn a_refusal_for_want_of_credentials_is_recognized() {
    assert_eq!(
        Verdict::from_reply(b"HTTP/1.1 401 Unauthorized\r\n\r\n"),
        Verdict::Unauthorized,
    );
    assert_eq!(
        render(&Verdict::Unauthorized),
        "the API refused the event: no credentials were sent with it",
    );
}

/// A reply does not have to be only a status line: headers and a body follow it in the same buffer,
/// and only the first line is read.
#[test]
fn a_status_line_is_read_out_of_whatever_follows_it() {
    let whole = b"HTTP/1.1 201 Created\r\n\
                  Content-Type: application/json\r\n\
                  Content-Length: 11\r\n\
                  \r\n\
                  {\"ok\":true}";
    assert_eq!(Verdict::from_reply(whole), Verdict::Stored);
}

/// Something that is not a status line is the captive-portal case: on a network with a login page,
/// the first fifteen bytes of the answer are a `<!DOCTYPE`. These are all whole replies — every one of
/// them has bytes and none of them has a status line in them — and they must not be confused with a
/// reply that has not finished arriving, which is [`Verdict`]'s problem and not this one's.
#[test]
fn something_that_is_not_a_status_line_is_reported_as_such() {
    let not_status_lines: [&[u8]; 6] = [
        b"<!DOCTYPE html>\r\n\r\n<html>",
        b"HTTP/1.1\r\n\r\n",
        b"HTTP/1.1 20x Created\r\n\r\n",
        // Four digits, which is a length or a version rather than a status code.
        b"HTTP/1.1 2011 Created\r\n\r\n",
        b"{\"ok\":true}",
        b"  HTTP/1.1 401 Unauthorized\r\n\r\n",
    ];

    for line in not_status_lines {
        assert_eq!(Verdict::from_reply(line), Verdict::NotTheApi, "{line:?}",);
    }

    assert_eq!(
        render(&Verdict::NotTheApi),
        "what answered was not the API: the first line was not a status line",
    );
}

/// Every other status is named as a number rather than guessed at, because a status this firmware
/// does not know is a status somebody has to go and read about.
#[test]
fn an_unnamed_status_is_reported_as_the_number_it_is() {
    for status in [200u16, 204, 301, 404, 429, 500, 503] {
        let reply = format!(
            "HTTP/1.1 {status} Something

"
        );

        assert_eq!(
            Verdict::from_reply(reply.as_bytes()),
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
