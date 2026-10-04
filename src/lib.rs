//! Firmware for the `esp32c6` proof of concept.
//!
//! The proof of concept is the network and then the time: [`wifi`] brings up the radio and hands
//! back a stack, [`ntp`] asks a time server what time it is over that stack, and [`clock`] is where
//! the answer is kept. [`status`] puts the three together into the line the binary in
//! `src/bin/main.rs` prints. Nothing in this crate is a reusable library.

#![no_std]
// `static_cell::make_static!` is built out of `impl Trait` in a type alias, which is what the pinned
// nightly is for. Without it the network stack's static storage would need the hand-rolled
// `StaticCell` dance that esp-hal's own examples do.
#![feature(type_alias_impl_trait)]

pub mod clock;
pub mod ntp;
pub mod status;
pub mod wifi;
