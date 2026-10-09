//! What the firmware decides for itself, decided here so that something can check it.
//!
//! This crate is the part of the firmware that decides rather than talks to hardware, and it is a
//! crate of its own because nothing in `src/` can be compiled for a host: every module there depends
//! on `esp-hal`, on the network stack, or on a scheduler that exists only on this chip, so a test on
//! any of them needs a board. This one is `#![no_std]` with no dependencies, so it builds for the
//! chip and for the host alike, and `tools/lib/firmware-tests.mjs` runs its tests wherever the gate
//! runs.
//!
//! **What is here, in four parts:**
//!
//! - [`address`] — an address, and how one is written.
//! - [`clock`] — what the firmware believes the time is, how it counts on, and how either is
//!   written. The shape the firmware publishes is [`Time`]; [`Clock`] is what the state line
//!   renders it as.
//! - [`ntp`] — what an SNTP packet off the network means, and what getting one came to when it did
//!   not.
//! - [`event`] — the body of a reported event, and how a reply is rendered into a log.
//!
//! Everything is re-exported at the root, so `use poc_domain::Time` is the import a caller wants and
//! `poc_domain::clock::Time` is where to read about it.
//!
//! **Deliberately not here:** what the radio is doing, and what an answer from the API means. A
//! failed join carries the driver's own `DisconnectReason` in `src/wifi.rs` and a status code is
//! reported as the number it is. **The HTTP framing is not here either**: `edge-http` writes the
//! request and parses the reply in `src/report.rs`, and what is logged there is a status code and a
//! body rather than a buffer to parse.

#![no_std]

pub mod address;
pub mod clock;
pub mod event;
pub mod ntp;

pub use address::Address;
pub use clock::{Clock, STALE_AFTER_SECS, Time, Timestamp, age};
pub use event::{EVENT_TELEMETRY, EVENTS_PATH, Event, LOGGED_LEN, REPORT_EVERY_SECS, logged};
pub use ntp::{Answer, Obstruction, Refusal, SNTP_LEN, sntp_reply, sntp_request};
