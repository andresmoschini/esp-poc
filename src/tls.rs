//! One HTTPS connection to the API: the trust anchor it is verified against, and the factory every
//! connection is made through.
//!
//! **A socket is not reusable.** `smoltcp` answers `connect` on an open socket with `InvalidState`,
//! so one held for the life of a task means every exchange after the first is refused before a
//! packet goes out; `edge-nal-embassy` builds it per `connect`, `edge-nal-tls` layers TLS on the
//! factory rather than on a borrowed socket, and `edge-http`'s `Connection` drives the connect
//! itself. **Do not reintroduce a socket that outlives one exchange.**
//!
//! **Expiry dates are not checked**, which is a consequence of how the `MbedTLS` in this tree was
//! built rather than a choice made here: the only way to enable `MBEDTLS_HAVE_TIME_DATE` is
//! `mbedtls-rs`'s `hook-wall-clock` feature, and enabling it makes `mbedtls-rs-sys` compile
//! `MbedTLS` from C source instead of using the libraries it ships. So the promise this firmware
//! keeps is "this chain leads to ISRG", not "this chain leads to ISRG and is current"; `src/clock.rs`
//! holds a real time, so comparing the certificate's own validity dates against it is the fix that
//! keeps the shipped libraries.

use core::ffi::CStr;

use edge_nal_embassy::{Tcp as EmbassyTcp, TcpBuffers};
use edge_nal_tls::{TlsConnector, TlsSocket};
use embassy_net::Stack;
use esp_hal::peripherals::{ADC1, RNG};
use esp_hal::rng::{Trng, TrngSource};
use mbedtls_rs::{Certificate, ClientSessionConfig, Tls};

/// The trust anchor: ISRG Root X1, in DER, in flash. See `certs/README.md` for where it came from,
/// what it is a promise about, and how to replace it.
///
/// [`Certificate::new_no_copy`] rather than [`Certificate::new`] because the bytes are already in
/// the image and never change: parsing them in place costs no heap at all, where a copy would cost
/// about a kilobyte and a byte on every boot. DER rather than PEM because that is the encoding
/// `MbedTLS` can parse that way at all.
const ROOT: &[u8] = include_bytes!("../certs/isrg-root-x1.der");

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
/// Public because [`crate::report`] sizes the scratch of its `Connection` from it: the head is parsed
/// out of that scratch, and a scratch smaller than the socket's would stop the read early.
pub const RX_LEN: usize = 1024;

/// Transmit buffer for the socket, in bytes, which has to hold the head and the body.
const TX_LEN: usize = 512;

/// The connection the reporter writes its request through, once the handshake is done.
///
/// A type alias rather than the type spelled out: the type is the awkward part.
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
/// If a `MbedTLS` instance already exists. [`crate::report`] calls this once, after DHCP is up, so
/// this is a statement about the program rather than something that can happen later.
pub fn instance(trng: Trng) -> &'static Tls<'static> {
    let trng: &'static mut Trng = static_cell::make_static!(trng);

    static_cell::make_static!(Tls::new(trng).expect("one MbedTLS instance, created once at boot"))
}

/// Enables the entropy source and builds the factory every exchange is made through.
///
/// A plain function rather than a task, for the same reason `src/ntp.rs` builds its socket outside
/// its task: `make_static!` inside a task body is a cycle the compiler cannot resolve, and a plain
/// function has no such future.
///
/// The source is enabled here — once an address is up — rather than at boot because enabling it is
/// what keeps a C6 from joining at all: the source is the SAR ADC, and [`TrngSource::new`]
/// reprograms it while the station has not even authenticated. DHCP up implies the station joined, so
/// from here on the radio is associated and the ADC is this exchange's to use.
///
/// The source is parked in a static rather than held because nothing outlives this call to keep it:
/// dropping it would switch the SAR ADC back off underneath the [`Trng`] that [`instance`] keeps,
/// and from there the key material would silently stop being key material.
///
/// # Panics
///
/// If the trust anchor in `certs/` does not parse — see [`connector`] — or if the entropy source
/// was not enabled, which here means the two lines below stopped agreeing with each other.
pub fn boot(
    rng: RNG<'static>,
    adc: ADC1<'static>,
    stack: Stack<'static>,
    name: &'static CStr,
) -> &'static TlsConnector<'static, EmbassyTcp<'static>> {
    let buffers = static_cell::make_static!(TcpBuffers::new());
    let _source: &'static mut TrngSource = static_cell::make_static!(TrngSource::new(rng, adc));
    let trng = Trng::try_new().expect("the entropy source above is enabled");
    let tls = instance(trng);

    connector(tls, buffers, stack, name)
}

/// The factory every connection is made through, built once and kept for the life of the program.
///
/// Building it once rather than per exchange is what lets a connection outlive this function: a
/// `TlsSocket` borrows the factory that made it, so a factory on the stack would return a socket
/// that could not leave. `TlsConnector::new` takes the configuration by reference and clones it,
/// which is why a local here is enough and no self-reference is involved.
///
/// The handshake is driven by whoever writes through the socket first — in practice `edge-http`'s
/// `Connection`, which also owns the connect — so the twenty-second budget for it and the line
/// saying what it settled on live on the exchange in `src/report.rs` rather than here. What stays
/// here is the part neither the factory nor the client knows about: which root the API's
/// certificate has to chain to.
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
