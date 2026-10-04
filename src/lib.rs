//! Firmware for the `esp32c6` proof of concept.
//!
//! The proof of concept is [`wifi`]: the binary in `src/bin/main.rs` brings the chip up and hands
//! over to it. Nothing in this crate is a reusable library.

#![no_std]
// `static_cell::make_static!` is built out of `impl Trait` in a type alias, which is what the pinned
// nightly is for. Without it the network stack's static storage would need the hand-rolled
// `StaticCell` dance that esp-hal's own examples do.
#![feature(type_alias_impl_trait)]

pub mod wifi;
