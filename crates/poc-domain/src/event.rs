//! What the body of a reported event looks like, and how a reply is rendered into a log.
//!
//! The body is JSON written by hand and a body that is wrong is wrong in a way nothing on the board
//! can see: a missing brace is a 400 from a server, and a timestamp in the wrong format is stored
//! as a string nobody can sort. [`logged`] is the other half of the same concern — bytes off the
//! network going into a serial log without garbling the terminal of whoever is reading it.
//!
//! **What an answer means is deliberately not here.** A status code is reported as the number it
//! is, because a mapping from numbers to sentences goes stale the day a status changes what it
//! means.

use core::fmt::{self, Write as _};

use crate::clock::Timestamp;

/// How often the firmware reports what it is doing.
///
/// Five minutes, which is [`REPORT_EVERY_SECS`] rather than a number in the task that waits: this is
/// the one interval in the firmware that is a decision about a third party rather than about a
/// timing here — a server that is going to answer with an authentication error is not going to
/// answer differently because it was asked twice as often — and a decision with a third party in it
/// is one that belongs where something can read it.
pub const REPORT_EVERY_SECS: u64 = 5 * 60;

/// The kind of every event this firmware reports.
///
/// One value rather than a field, because there is one kind: what this chip is doing right now. A
/// second kind would be a second type here, and the difference between "the chip is up" and "the
/// chip was up" is a difference in when it is sent rather than in what the API stores about it.
pub const EVENT_TELEMETRY: &str = "telemetry";

/// The path the API takes an event on.
pub const EVENTS_PATH: &str = "/events";

/// One event, as the JSON the API stores.
///
/// The four fields are the API's, in its order, and the payload is a string rather than an object:
/// the API stores whatever JSON the body carries as text, so nesting a JSON document inside one is
/// this firmware's business and not the schema's — a server that wants to read it back is a
/// different API than one that wants to keep it.
///
/// A `Display` rather than a serializer because there is no serializer here and no allocator either:
/// this crate is `#![no_std]` with no dependencies, and the body is small enough that writing it
/// with [`fmt::Write`] is a few lines rather than a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event<'a> {
    /// What this chip is called, which is a name the API stores beside the event.
    pub device_id: &'a str,

    /// When the event happened, as seconds since the Unix epoch.
    ///
    /// A count and not a string because the firmware does not format a time here: it has one
    /// number and this is where it is handed over. What the number becomes is [`Timestamp`]'s
    /// business, and it is [`Timestamp`]'s business because that is the part that cannot be checked
    /// by looking at it.
    pub timestamp_secs: u64,

    /// What kind of event this is, which is [`EVENT_TELEMETRY`] for everything this firmware sends.
    pub event_type: &'a str,

    /// The state line, rendered by `src/status.rs`.
    ///
    /// The whole sentence rather than the three fields behind it, so that what is stored is what the
    /// chip would have printed at that moment. Three fields in a payload would be a second way of
    /// writing the same line, and the two would drift.
    pub payload: &'a str,
}

impl fmt::Display for Event<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Every field below stays inside plain printable text with no quotes in it, which is the
        // one rule a JSON string has — a quote ends it. The id is hex from `src/report.rs`, the
        // timestamp is digits and fixed punctuation, the event type is a constant, and the payload
        // is a state line whose alphabet a test pins. A field that grows a quote fails that test
        // rather than corrupting a row in somebody's database.
        write!(
            f,
            "{{\"device_id\":{},\"timestamp\":\"{}\",\"event_type\":{},\"payload\":{}}}",
            Quoted(&self.device_id),
            Timestamp::at(self.timestamp_secs),
            Quoted(&self.event_type),
            Quoted(&self.payload),
        )
    }
}

/// A value inside a JSON string, quoted and nothing else.
///
/// No escaping, because there is nothing to escape: every sentence this crate can put in a body is
/// pinned by a test to hold no `"`, no `\` and nothing below `U+0020`, and the id and the event
/// type are a hex string and a constant. Quoting stays a wrapper rather than an `as_str`-and-paste
/// so that it cannot be forgotten at one of the call sites: the places above call this, and
/// nothing else in this crate builds a JSON string by hand.
///
/// Over any `Display` rather than over `&str` because the payload is a [`Status`] — a sentence
/// this crate formats — and there is no allocator here to turn a formatted value into a `&str`.
struct Quoted<'a>(&'a dyn fmt::Display);

impl fmt::Display for Quoted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The two quotes are the delimiters, and the thing being quoted is what goes between them.
        //
        // Written this way rather than into a `String` that is then quoted, because this crate has no
        // allocator: the value is formatted straight into the body.
        f.write_char('"')?;
        write!(f, "{}", self.0)?;
        f.write_char('"')
    }
}

/// How many bytes of a body [`logged`] will write.
///
/// 120, which is more than any message this API sends — its longest is `{"error":"Unauthorized"}` at
/// 24 — and short enough that a line with the greeting's worth of context in front of it stays
/// readable on a serial log. A Cloudflare error page gets cut, which costs nothing: its first 120
/// characters already say it is HTML.
pub const LOGGED_LEN: usize = 120;

/// The bytes of a body as something safe to write into a serial log.
///
/// Bytes off the network cannot go into a log as they are: they may not be text at all, and a
/// `0x00` or an escape sequence in a serial log garbles the terminal of whoever is reading it,
/// which destroys the very output the line exists to produce. So a body that is not UTF-8 is not
/// rendered — it is counted — and one that is has its control characters blanked, is cut at
/// [`LOGGED_LEN`], and is marked when it is cut, so that a truncated body does not read as a
/// complete one.
///
/// A `Display` rather than a `String` for the reason [`crate::clock::age`] gives: the truncation and the counting
/// are decisions, and a decision made in a private function returning a `String` would be one this
/// crate has no allocator to make.
#[must_use]
pub fn logged(body: &[u8]) -> impl fmt::Display {
    Logged(body)
}

/// A body that knows how to write itself safely, in [`logged`].
struct Logged<'a>(&'a [u8]);

impl fmt::Display for Logged<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("(no body)");
        }

        let Ok(text) = core::str::from_utf8(self.0) else {
            return write!(f, "({} non-utf8 bytes)", self.0.len());
        };

        let mut cut = text.len().min(LOGGED_LEN);

        while !text.is_char_boundary(cut) {
            cut -= 1;
        }

        for c in text[..cut].chars() {
            // A newline in a body would split the log line it is printed on, and an escape would
            // reach the reader's terminal. Bodies from this API are JSON without either, so
            // blanking is a guard rather than a rendering.
            f.write_char(if c.is_control() { ' ' } else { c })?;
        }

        if text.len() > LOGGED_LEN {
            f.write_str("…")?;
        }

        Ok(())
    }
}
