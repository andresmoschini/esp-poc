//! What the firmware has to say about the network, decided here and printed there.
//!
//! Two decisions live in this crate. The first is how an address is written: the greeting in
//! `src/bin/main.rs` and the report in `src/wifi.rs` both print one, and they used to be two pieces of
//! formatting that could drift apart. The second is what a failed join says: the radio names about
//! fifty reasons, and printing its own words for them answers the question "what did the hardware say"
//! rather than the question an operator is asking, which is what to do next.
//!
//! It is a crate of its own because this is the only part of the firmware that can be tested at all.
//! `src/wifi.rs` and `src/bin/main.rs` both depend on `esp-hal`, which exists only for this chip, so
//! neither of them can be compiled for a host — and a test on a microcontroller needs a board or a
//! simulator, which the gate does not have. This crate is `#![no_std]` with no dependencies, so it
//! builds for the chip and for the host alike, and `tests/report.rs` runs on whichever machine is
//! running the gate.
//!
//! It knows nothing about Wi-Fi. `esp_radio::wifi::DisconnectReason` is translated into a [`Reason`]
//! in `src/wifi.rs`, which is the only file allowed to depend on the radio, so that a change in the
//! driver costs that one match arm rather than this crate's API.

#![no_std]

use core::fmt;
use core::net::Ipv4Addr;

/// An address the stack is holding, with the prefix length that goes with it.
///
/// Written the way a router writes one — `192.168.0.225/24` — because that is the form both the
/// greeting and the report print, and the prefix belongs to the address rather than to whichever of
/// them is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    /// The address itself.
    pub ip: Ipv4Addr,

    /// How many of its leading bits name the network rather than the host.
    pub prefix_len: u8,
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.ip, self.prefix_len)
    }
}

/// Why joining a network did not work, in as few words as the radio allows.
///
/// The four cases below are the ones that want four different things done about them; everything else
/// the radio can report is [`Reason::Other`], which is a deliberate admission rather than a guess.
/// A wrong guess here is worse than a missing one, because it sends whoever is reading the serial
/// output after the wrong problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Nothing with that name was heard.
    ///
    /// One of three things: the name is wrong, the network is not there, or the station is too far
    /// from it to be answered. The scan printed alongside this failure is what tells those apart.
    NoSuchNetwork,

    /// The network was heard and refused how this station asked to join it.
    ///
    /// In practice the password, or a security type that WPA2 does not cover.
    SecurityRefused,

    /// The exchange started and the other end stopped answering.
    ///
    /// This is what a station too far from its access point looks like from its own side: strong
    /// enough to hear a beacon, too weak to finish a handshake.
    NoAnswer,

    /// The link came up and then went down.
    LinkLost,

    /// The radio reported something this firmware does not name.
    Other,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(write_reason(*self))
    }
}

/// A join that failed, with the signal the radio last measured.
///
/// The signal is there because the two failures that look identical in the radio's own words are not
/// identical to fix: a wrong password and a station too far away both arrive as an exchange that
/// stops, and the dBm reading is what separates them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    /// What went wrong.
    pub reason: Reason,

    /// How strong the signal was, in dBm, when the radio measured one.
    pub signal: Option<i8>,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.signal {
            Some(dbm) => write!(f, "{} (signal {dbm} dBm)", self.reason),
            None => fmt::Display::fmt(&self.reason, f),
        }
    }
}

/// The sentence for a reason, on its own.
///
/// It is a function rather than an arm of the `Display` above so that adding a case to the enum stays
/// one edit: [`Failure`] reaches it for the arm that has no signal to add, and a match written twice
/// is a match that gets changed in one of the two places.
fn write_reason(reason: Reason) -> &'static str {
    match reason {
        Reason::NoSuchNetwork => "nothing with that name was heard",
        Reason::SecurityRefused => "the network refused these credentials",
        Reason::NoAnswer => "the network stopped answering partway through",
        Reason::LinkLost => "the link came up and then went down",
        Reason::Other => "the radio reported a reason this firmware does not name",
    }
}
