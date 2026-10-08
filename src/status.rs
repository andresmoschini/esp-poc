//! What the firmware says about itself, from the three modules that each know one thing.
//!
//! Three tasks produce the three facts on the state line — the radio in [`crate::wifi`]'s task, the
//! time in [`crate::clock`], the address in the stack the same task hands back — and none of them
//! can return to the loop in `src/bin/main.rs` that prints them. This module is the one place that
//! asks all three and puts the answers together, so that the greeting is a call rather than an
//! argument list assembled next to the printer.
//!
//! It is here rather than in `src/bin/main.rs` for a second reason: that file is generated, and
//! esp-generate overwrites it. Anything worth keeping in it has to be put back by hand after a
//! regeneration, which is recorded in AGENTS.md; this is not in that list, because it is not
//! regenerated.
//!
//! There is no logic here that a host can check, and that is the design: the wording of the states
//! lives next to the hardware that decides them — the radio's in [`crate::wifi`], the time's in
//! `poc-report` — and what this module does is order them. `embassy-net` has no host build, so there
//! is no way to ask that stack for its address off a board, and the line as a whole can only be read
//! on one.

use core::fmt;

use embassy_net::Stack;
use poc_report::{Address, Time};

use crate::{clock, wifi};

/// Everything the firmware knows about itself, right now, as the line it prints.
///
/// The order is the decision as much as the wording is: the time first, because a reader who came to
/// see what time it is has their answer in the first field and can stop reading; then the address,
/// which is the next thing they want; and the state of the two mechanisms last, because that is what
/// they read when one of the first two is wrong, and by then they are looking for it.
///
/// A struct rather than three format arguments because these three come from three tasks, and three
/// arguments assembled in the greeting is three places for the log and the state to disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Where the time came from, and whether it can still be believed.
    pub time: Time,

    /// What the radio is doing, and why.
    pub link: wifi::Link,

    /// The address, or `None` while DHCP has not produced one.
    pub address: Option<Address>,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, ", self.time)?;

        match self.address {
            Some(address) => write!(f, "{address}, ")?,
            // Said rather than left out, because a missing clause reads as a formatting mistake and
            // the state of the radio is the answer to it.
            None => f.write_str("no address yet, ")?,
        }

        write!(f, "wifi: {}", self.link)
    }
}

/// Everything the firmware knows about itself, right now.
///
/// Never waits and never fails: reading the stack's configuration is a lookup rather than a wait,
/// and everything else is a load behind a brief critical section. That is what lets the greeting ask
/// twice a second without knowing whether there is a network, whether there is an address, or
/// whether anything has set the clock — all three of which are answers rather than questions this
/// function waits for.
#[must_use]
pub fn report(stack: Option<Stack<'_>>) -> Status {
    Status {
        time: clock::time(),
        link: wifi::link(),
        address: stack
            .and_then(|stack| stack.config_v4())
            .map(|config| wifi::address(&config)),
    }
}
