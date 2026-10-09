//! An address, and how one is written.
//!
//! The one place either is decided, because the greeting and the reported event both print one and
//! two renderings of an address are two ways for them to disagree.

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
