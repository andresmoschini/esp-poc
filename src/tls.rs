//! One HTTPS connection to the API: the trust anchor it is verified against, and the factory every
//! connection is made through.
//!
//! ## Why this goes through `edge-nal`
//!
//! The first version of this file talked to `embassy-net` directly: a `TcpSocket` owned by the
//! reporting task, `connect` called on it, and a TLS session over it. That is not much code, but two
//! of its properties were wrong and one of them was a bug this repository shipped and then found:
//!
//! - **A socket is not reusable.** `smoltcp` answers `connect` on an open socket with
//!   `InvalidState`, so a socket has to be built per connection and dropped after. The task was
//!   holding one, which meant every report after the first failed before a packet went out. The
//!   buffers then had to be built separately and threaded through the reporter to make that work.
//! - **TLS over it is a second API.** The session borrows the socket, borrows a NUL-terminated server
//!   name, and borrows a `&'static mut` to the RNG, and every one of those lifetimes had to be
//!   spelled out at every use.
//!
//! `edge-nal-embassy` builds the socket per `connect` and returns its buffers to a pool when the
//! connection is dropped, which makes both of the first two facts the library's problem rather than
//! this file's. `edge-nal-tls` layers the TLS session on top of a factory rather than on a borrowed
//! socket. What is left here is the part neither knows about: which root the API's certificate has
//! to chain to, and what to say when the handshake does not happen.
//!
//! `mbedtls-rs` is still a direct dependency, and deliberately so. It is what puts the bytes on the
//! wire, `edge-nal-tls` re-exports it, and the trust anchor is built from its types — but nothing
//! here names `embassy_net::tcp` any more.
//!
//! ## What is verified, and what is not
//!
//! Chain, signatures and hostname are checked: the certificate the API presents has to lead to
//! `certs/isrg-root-x1.der` and has to name the host this firmware asked for. **Expiry dates are not
//! checked.** That is not a choice made here but a consequence of how the `MbedTLS` in this tree was
//! built: `MBEDTLS_HAVE_TIME_DATE` is compiled out unless `mbedtls-rs`'s `hook-wall-clock` feature is
//! on, and turning it on changes the C library's configuration, which makes `mbedtls-rs-sys`
//! discard the static libraries it ships and compile `MbedTLS` from C source instead — needing
//! `CMake`, Clang and a RISC-V C cross-compiler, none of which this project otherwise requires. So the
//! promise this firmware can keep is "this chain leads to ISRG", not "this chain leads to ISRG and
//! is current". `src/clock.rs` already holds a real time from SNTP, so enabling the hook and
//! supplying it is the fix; it is a build-environment change rather than a code change, which is why
//! it is its own piece of work.

use core::ffi::CStr;
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use defmt::{error, info};
use edge_nal::TcpConnect as _;
use edge_nal_embassy::{Tcp as EmbassyTcp, TcpBuffers};
use edge_nal_tls::{TlsConnector, TlsSocket};
use embassy_net::Stack;
use embassy_time::{Duration, with_timeout};
use esp_hal::rng::Trng;
use mbedtls_rs::{Certificate, ClientSessionConfig, Tls};

/// The trust anchor: ISRG Root X1, in DER, in flash. See `certs/README.md` for where it came from,
/// what it is a promise about, and how to replace it.
///
/// [`Certificate::new_no_copy`] rather than [`Certificate::new`] because the bytes are already in
/// the image and never change: parsing them in place costs no heap at all, where a copy would cost
/// about a kilobyte and a byte on every boot. DER rather than PEM because that is the encoding
/// `MbedTLS` can parse that way at all.
const ROOT: &[u8] = include_bytes!("../certs/isrg-root-x1.der");

/// How long the handshake may take before it is given up on.
///
/// Its own budget rather than the reporter's per-step one because it is not the same kind of wait:
/// every other step is a request going out or a reply coming back over an established connection,
/// while this one is the API proving who it is, which on a 160 MHz RISC-V running `MbedTLS`' own
/// arithmetic rather than the chip's accelerators is seconds rather than milliseconds. The handshake
/// is also renegotiated from scratch every five minutes, because the connection is built fresh each
/// time and paid for in full every time.
///
/// Twenty seconds is a ceiling, not a measurement: the observed handshake on the C3 completes well
/// inside the reporter's own five-second budget, so this only bounds the failure where nothing comes
/// back at all.
const HANDSHAKE: Duration = Duration::from_secs(20);

/// How many socket buffer pairs this reporter keeps.
///
/// One, because there is one reporter that makes one connection at a time. This is the number
/// `embassy-net`'s fixed socket set has to give back, and the pool is what gives it back: a connection
/// that is dropped returns its pair here, so the next one can have it.
const POOL: usize = 1;

/// Receive buffer for the socket, in bytes.
///
/// Larger than any reply this API gives — the largest is a Cloudflare error page of a few hundred
/// bytes. `smoltcp` drops a datagram that does not fit rather than truncating it, so a buffer sized
/// to the status line alone would throw away every body there is.
///
/// It is also large enough for the largest record TLS will deliver in one piece. A TLS record is at
/// most 16 KiB by `MbedTLS`' default, and the socket hands the session whatever `mbedtls_ssl_read`
/// returns, which is one record's worth of plaintext — so a peer whose first record is larger than
/// this would have it split across reads rather than truncated. Measured on the deployed Worker: one
/// 634-byte reply, arriving in one read.
///
/// Public because [`crate::report`] sizes its own reply buffer from it: a buffer smaller than the
/// socket's would stop the read early and a reply that did not fit is not a shorter reply.
pub const RX_LEN: usize = 1024;

/// Transmit buffer for the socket, in bytes, which has to hold the head and the body.
const TX_LEN: usize = 512;

/// The connection the reporter writes its request through, once the handshake is done.
///
/// A type alias rather than the type spelled out at each use: the type is the awkward part, and three
/// of them in [`crate::report`] is three places to get subtly wrong.
pub type Stream<'a> = TlsSocket<'a, edge_nal_embassy::TcpSocket<'a>>;

/// The one `MbedTLS` instance this firmware has, holding the entropy source it draws from.
///
/// `Trng` is moved in rather than borrowed because `MbedTLS` wants a `&'static mut` to a `CryptoRng`:
/// there is exactly one of these for the life of the program, so the chip's hardware RNG is parked
/// in a static here and never moved again. The alternative — the safe constructor that takes a
/// shorter borrow — is `unsafe` precisely because the pointer outlives the borrow, which is not a
/// trade this firmware needs to make.
///
/// It is returned as a `&'static` rather than as a value because [`connector`] borrows it and the
/// factory has to outlive every exchange.
///
/// # Panics
///
/// If a `MbedTLS` instance already exists. [`crate::report::start`] calls this once, at boot, so this
/// is a statement about the program rather than something that can happen later.
pub fn instance(trng: Trng) -> &'static Tls<'static> {
    let trng: &'static mut Trng = static_cell::make_static!(trng);

    static_cell::make_static!(Tls::new(trng).expect("one MbedTLS instance, created once at boot"))
}

/// The factory every connection is made through, built once and kept for the life of the program.
///
/// Building it once rather than per exchange is what lets a connection outlive this function: a
/// `TlsSocket` borrows the factory that made it, so a factory on the stack would return a socket
/// that could not leave. `TlsConnector::new` takes the configuration by reference and clones it,
/// which is why a local here is enough and no self-reference is involved.
///
/// # Panics
///
/// If the trust anchor in `certs/` does not parse. Those bytes are a constant this repository ships,
/// so a failure here is a defect in the repository rather than a condition on the network, and it
/// is why this returns a value rather than an `Option`.
pub fn connector(
    tls: &'static Tls<'static>,
    pool: &'static TcpBuffers<POOL, TX_LEN, RX_LEN>,
    stack: Stack<'static>,
    name: &'static CStr,
) -> &'static TlsConnector<'static, EmbassyTcp<'static>> {
    let root = Certificate::new_no_copy(ROOT)
        .expect("the trust anchor in certs/ is a certificate this build shipped");

    // `ClientSessionConfig::new()` is the whole struct with the defaults, and the defaults are the
    // ones wanted here: `AuthMode::Required`, so a certificate that does not verify aborts the
    // handshake rather than being logged after the fact, and TLS 1.2 as the floor. Only the trust
    // anchor and the name are added.
    let config = ClientSessionConfig {
        ca_chain: Some(root),
        server_name: Some(name),
        ..ClientSessionConfig::new()
    };

    static_cell::make_static!(TlsConnector::new(
        tls.reference(),
        EmbassyTcp::new(stack, pool),
        &config
    ))
}

/// Connects to the API at `address` and negotiates TLS with it.
///
/// Everything about the socket is the factory's: one is built for this call and returned to the pool
/// when the returned stream is dropped, which is what makes a second call five minutes later work at
/// all. What this adds is the one thing the factory cannot do — give the handshake its own timeout
/// and say what it settled on.
///
/// The handshake is driven explicitly rather than left to the first read, because a lazy handshake
/// would report its failure as a failed _read_, a long way from the certificate that caused it, and
/// because the negotiation it agreed on is worth a line in the log.
///
/// The server name is not a parameter: the factory holds it, from [`connector`], because `MbedTLS`
/// wants it NUL-terminated for both SNI and the certificate's hostname check.
///
/// Returns `None` having already said why in the log, because every way this can fail is a
/// different problem with a different fix: a connection that is refused, a certificate this
/// firmware does not trust, and a handshake that ran out of time are three of them.
pub async fn open(
    connector: &'static TlsConnector<'static, EmbassyTcp<'static>>,
    address: Ipv4Addr,
    port: u16,
) -> Option<Stream<'static>> {
    let mut socket = match connector
        .connect(SocketAddr::new(IpAddr::V4(address), port))
        .await
    {
        Ok(socket) => socket,
        Err(e) => {
            error!("the API would not accept the connection: {:?}", e);

            return None;
        }
    };

    let session = socket.session_mut();

    match with_timeout(HANDSHAKE, session.connect()).await {
        Err(_) => {
            error!(
                "the API did not complete a TLS handshake in {} seconds",
                HANDSHAKE.as_secs()
            );

            None
        }
        Ok(Err(e)) => {
            error!("the API's TLS handshake failed: {:?}", e);

            None
        }
        Ok(Ok(())) => {
            // The version and the verification flags together are what says the API was checked and
            // not merely reached: a zero flag is the only value that means the chain, the signature
            // and the hostname all agreed, and the version says what was agreed about. Neither
            // replaces the log line the refusal itself produces.
            info!(
                "the API's certificate verified: {:?}, flags {:#x}",
                session.tls_version(),
                session.tls_verification_details()
            );

            Some(socket)
        }
    }
}
