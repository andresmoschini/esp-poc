//! The TLS under one HTTPS exchange: the trust anchor the API's certificate is checked against, the
//! entropy `MbedTLS` is fed from, and the handshake that turns a socket into a verified stream.
//!
//! This is the layer between a TCP connection and an HTTP request, so it is a separate file from
//! [`crate::report`]: that one decides *what* to say to the API every five minutes, and this one says
//! whether the thing answering is the API. Only one [`Tls`] may exist at a time, which is why
//! [`instance`] is called once from [`crate::report::start`] rather than per exchange — the `Tls`
//! itself is then carried by the reporting task and handed out as a [`TlsReference`].
//!
//! ## What is verified, and what is not
//!
//! Chain, signatures and hostname are checked: the certificate the API presents has to lead to
//! `certs/isrg-root-x1.der` and has to name the host this firmware asked for. **Expiry dates are not
//! checked.** That is not a choice made here but a consequence of how the `MbedTLS` in this tree was
//! built: `MBEDTLS_HAVE_TIME_DATE` is compiled out unless `mbedtls-rs`'s `hook-wall-clock` feature is
//! on, and turning it on changes the C library's configuration, which makes `mbedtls-rs-sys`
//! discard the static libraries it ships and compile `MbedTLS` from C source instead — needing
//! `CMake`, Clang and a RISC-V cross-compiler, none of which this project otherwise requires. So the
//! promise this firmware can keep is "this chain leads to ISRG", not "this chain leads to ISRG and
//! is current". `src/clock.rs` already holds a real time from SNTP, so enabling the hook and
//! supplying it is the fix; it is a build-environment change rather than a code change, which is why
//! it is its own piece of work.

use core::ffi::CStr;

use defmt::{error, info};
use embassy_net::tcp::TcpSocket;
use embassy_time::{Duration, with_timeout};
use esp_hal::rng::Trng;
use mbedtls_rs::{Certificate, ClientSessionConfig, Session, SessionConfig, Tls, TlsReference};

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
/// is also given a fresh [`Session`] each time, so it is renegotiated from scratch every five minutes
/// and paid for in full every time.
///
/// Twenty seconds is a ceiling, not a measurement: the observed handshake on the C3 completes well
/// inside the reporter's own five-second budget, so this only bounds the failure where nothing comes
/// back at all.
const HANDSHAKE: Duration = Duration::from_secs(20);

/// A TLS session over one socket, verified against the root in `certs/`.
///
/// Two lifetimes rather than one because the socket's own lifetime is not the session's: the socket is
/// borrowed for as long as the session, but it was created earlier and outlives the session by
/// however long it takes to drop. A type alias rather than the session type spelled out at each use,
/// because the lifetimes are the awkward part and four of them in [`crate::report`] is four places
/// to get subtly wrong.
pub type Stream<'a, 'socket> = Session<'a, &'a mut TcpSocket<'socket>>;

/// The one `MbedTLS` instance this firmware has, holding the entropy source it draws from.
///
/// `Trng` is moved in rather than borrowed because `MbedTLS` wants a `&'static mut` to a
/// `CryptoRng`: there is exactly one of these for the life of the program, so the chip's hardware
/// RNG is parked in a static here and never moved again. The alternative — the safe constructor that
/// takes a shorter borrow — is `unsafe` precisely because the pointer outlives the borrow, which is
/// not a trade this firmware needs to make.
///
/// # Panics
///
/// If a `MbedTLS` instance already exists. [`crate::report::start`] calls this once, at boot, so this
/// is a statement about the program rather than something that can happen later.
pub fn instance(trng: Trng) -> Tls<'static> {
    let trng: &'static mut Trng = static_cell::make_static!(trng);

    Tls::new(trng).expect("one MbedTLS instance, created once at boot")
}

/// Opens a TLS session over `socket` and negotiates it with the API named `name`.
///
/// Nothing is left half-open on failure: a session that could not be created or could not complete a
/// handshake is dropped, which drops the borrow of `socket` and so returns the caller a usable TCP
/// connection rather than a TLS one that is stuck partway through a handshake.
///
/// `name` is the host as a NUL-terminated string, which is the form `MbedTLS` wants for both SNI and
/// the certificate's hostname check, and it must outlive the session because it is part of the
/// configuration the session borrows. [`crate::report`] builds it once for the life of the program,
/// from the same build-time value it resolves and prints.
///
/// Returns `None` having already said why in the log, because every way this can fail is a
/// different problem with a different fix: a certificate this firmware does not trust, a name the
/// certificate does not carry, and a handshake that ran out of time are three of them.
pub async fn open<'a, 'socket>(
    tls: TlsReference<'a>,
    socket: &'a mut TcpSocket<'socket>,
    name: &'a CStr,
) -> Option<Stream<'a, 'socket>> {
    let root = match Certificate::new_no_copy(ROOT) {
        Ok(root) => root,
        Err(e) => {
            error!("the API's root certificate did not parse: {:?}", e);

            return None;
        }
    };

    // `ClientSessionConfig::new()` is the whole struct with the defaults, and the defaults are the
    // ones wanted here: `AuthMode::Required`, so a certificate that does not verify aborts the
    // handshake rather than being logged after the fact, and TLS 1.2 as the floor. Only the trust
    // anchor and the name are added.
    let config = SessionConfig::Client(ClientSessionConfig {
        ca_chain: Some(root),
        server_name: Some(name),
        ..ClientSessionConfig::new()
    });

    let mut session = match Session::new(tls, &mut *socket, &config) {
        Ok(session) => session,
        Err(e) => {
            error!("the TLS session could not be set up: {:?}", e);

            return None;
        }
    };

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

            Some(session)
        }
    }
}
