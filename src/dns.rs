//! The one name lookup both network clients share.
//!
//! [`crate::ntp`] asks a time server and [`crate::report`] asks an API, and both start the same
//! way: wait out a query for an A record and take the IPv4 address out of the answer. That is one
//! exchange with one shape, so it is one function rather than two copies of one.
//!
//! What is not shared is what a refusal means, so the sentences stay with the callers: a name that
//! does not resolve is DNS or the network for one and a name this cannot be reached at for the
//! other, and the name is in both sentences because a log line about a name is not answerable
//! without the name.

use core::net::Ipv4Addr;

use embassy_net::dns::DnsQueryType;
use embassy_net::{IpAddress, Stack};
use embassy_time::{Duration, with_timeout};

/// How long a lookup may take before it is given up on.
///
/// Five seconds, as in both callers before they shared this: a name that does not resolve and a
/// server that does not answer are different problems, and the timeout on the lookup is what tells
/// them apart in the log.
const TIMEOUT: Duration = Duration::from_secs(5);

/// What a lookup came to when it did not produce an address.
///
/// The sentences for these live with the callers rather than here, because the same outcome wants
/// different words from a time client and from a reporter.
#[derive(Debug)]
pub enum Failure {
    /// The lookup did not finish in time.
    TimedOut,

    /// The resolver refused the question.
    Refused(embassy_net::dns::Error),

    /// The answer held nothing this firmware can send to: empty, or IPv6-only.
    NoIpv4,
}

/// The IPv4 address of `name`, once DHCP has given the resolver some to ask.
///
/// An A record is a question about IPv4 and this firmware has no other protocol to send over, so an
/// answer that is empty or IPv6-only is not a name that did not exist: it is a name this cannot be
/// reached at, which is [`Failure::NoIpv4`] rather than one of the two refusals.
///
/// # Errors
///
/// A [`Failure`]: the lookup timed out, the resolver refused it, or the answer held nothing this
/// firmware can send to. What each of those wants done about it is the caller's to say.
pub async fn resolve(stack: &Stack<'static>, name: &str) -> Result<Ipv4Addr, Failure> {
    let found = match with_timeout(TIMEOUT, stack.dns_query(name, DnsQueryType::A)).await {
        Err(_) => return Err(Failure::TimedOut),
        Ok(Err(e)) => return Err(Failure::Refused(e)),
        Ok(Ok(found)) => found,
    };

    if let Some(IpAddress::Ipv4(address)) = found.first() {
        return Ok(*address);
    }

    Err(Failure::NoIpv4)
}
