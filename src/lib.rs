//!
//! The proof of concept is the network and then the time: [`wifi`] brings up the radio and hands
//! back a stack, [`ntp`] asks a time server what time it is over that stack, and [`clock`] is where
//! the answer is kept. [`status`] puts the three together into the line the binary in
//! `src/bin/main.rs` prints, [`tls`] is what makes the API's answer to a report trustworthy, and
//! [`report`] sends that line over HTTPS every five minutes. Nothing in this crate is a reusable
//! library.

#![no_std]
// `static_cell::make_static!` is built out of `impl Trait` in a type alias, which is what the pinned
// nightly is for. Without it the network stack's static storage would need the hand-rolled
// `StaticCell` dance that esp-hal's own examples do.
#![feature(type_alias_impl_trait)]

use embassy_time::Duration;

/// How long any single step of a network exchange may take.
///
/// One value for the three steps that share it — resolving a name, sending a request, reading a
/// reply — because one timeout is what they all mean. They are timed separately rather than the
/// exchange as a whole so that a log that says "timed out" also says which of them did: a name that
/// does not resolve and a server that does not answer are different problems, and the fixes are
/// opposite. A per-module constant spelled the same number three times is three places for that to
/// be right in and one place too many to look in.
pub const TIMEOUT: Duration = Duration::from_secs(5);

pub mod clock;
pub mod dns;
pub mod ntp;
pub mod report;
pub mod status;
pub mod tls;
pub mod wifi;
