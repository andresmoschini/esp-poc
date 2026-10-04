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
//! There is no logic here, and that is the design: whether a state is worth saying, how old an
//! answer has to be before the time it set stops being called current, and how a failure reads all
//! belong to `poc-report`, where a host can test them. This module reads three answers and hands
//! them over. What cannot be tested is also what it does — `embassy-net` has no host build, so there
//! is no way to ask that stack for its address off a board.

use embassy_net::Stack;
use poc_report::Status;

use crate::{clock, wifi};

/// Everything the firmware knows about itself, right now.
///
/// Never waits and never fails: reading the stack's configuration is a lookup rather than a wait,
/// and everything else is a load of an atomic. That is what lets the greeting ask twice a second
/// without knowing whether there is a network, whether there is an address, or whether anything has
/// set the clock — all three of which are answers rather than questions this function waits for.
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
