//! The proof of concept: join a Wi-Fi network, take an address over DHCP, and print it.
//!
//! Wi-Fi on this chip is three pieces the project did not have before, and each one is a
//! precondition of the next:
//!
//! - `esp-radio` drives the radio. On the `ESP32-C6` it is a device on the internal SDIO bus, so
//!   unlike the original `ESP32` there are no pins to choose and no antenna to configure.
//! - `esp-rtos` is the preemptive scheduler that driver needs. It will not start without one, and
//!   the scheduler has to be running *before* the radio is initialized, which is why [`join`] is
//!   called after `esp_rtos::start` rather than before.
//! - `embassy-net` is the TCP/IP stack that turns a joined network into an IP address. It runs on
//!   the executor that an `async` `#[esp_hal::main]` sets up.
//!
//! Nothing here waits for the network: [`join`] starts three tasks and returns, so the firmware
//! keeps doing whatever it was doing while the radio does its work in the background. What
//! `join` returns is the handle to the network stack, which is what the rest of the firmware needs
//! — an SNTP client runs on it, in [`crate::ntp`] — and which is `Copy`, so a task can take its own
//! copy of it.
//!
//! ## Saying what the radio is doing
//!
//! The radio runs in a task and the greeting runs in another one, so nothing here can return the
//! state of the link to it. Instead this file publishes it as a [`Link`] — what it is doing, and why
//! it stopped if it did — and [`link`] hands it to [`crate::status`], which prints it twice a
//! second. A build with no credentials says so on that line, rather than leaving a reader to work
//! out from a missing address that nothing was ever attempted.
//!
//! The state is published as one word rather than as the enum itself because this core has 32-bit
//! atomics and no 64-bit ones, and because a value published as several words is a value whose parts
//! can be read at different moments. [`Link::to_word`] is the other end of that.
//!
//! ## Credentials
//!
//! The SSID and password are compiled in from the `WIFI_SSID` and `WIFI_PASSWORD` environment
//! variables, which is the only way to get them into firmware that has no filesystem to read.
//! Anything a user types would end up in a tracked file, so the values live in the `[env]` section
//! of `esp-config.toml`, which `.gitignore` keeps out of the repository.
//!
//! They are read with `option_env!` rather than `env!` on purpose: a build with no credentials
//! still has to build, because the gate and CI are such a build. There, [`join`] says so and
//! returns `None`, and the rest of the firmware runs exactly as it did before Wi-Fi existed.

use core::sync::atomic::{AtomicU32, Ordering};

use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources, StaticConfigV4};
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig, DisconnectReason,
    Interface, Password, Ssid, WifiController, WifiError, scan::ScanConfig, sta::StationConfig,
};
use poc_report::{Address, JoinFailure, Link, Reason};

/// The SSID of the network to join, read from the environment when this was compiled.
const SSID: Option<&str> = option_env!("WIFI_SSID");

/// The password of that network, likewise. Empty for an open network.
const PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

/// How long to wait between attempts to join, whether the last one failed or succeeded.
const RETRY: Duration = Duration::from_secs(5);

/// How many access points to name when a connection attempt fails.
const NEIGHBORS: usize = 10;

/// Sockets the network stack is sized for.
///
/// Three of them are taken before anything in this file asks for one: DHCP takes a socket when the
/// stack is first configured, the DNS resolver takes one when the stack is built, and the SNTP
/// client in `src/ntp.rs` takes the third. The fourth is the point — `smoltcp`'s socket set is a
/// fixed-size array that panics when it is full rather than refusing the socket that does not fit,
/// so this is the number that decides whether the next thing to want a socket works at all.
const SOCKETS: usize = 4;

/// What the radio is doing, published for [`link`] to read.
///
/// One atomic rather than a lock around a [`Link`]: the greeting reads this from a different task
/// than the one that writes it, twice a second, and a lock to protect one small answer is a lock
/// the radio could block on while it is trying to join. The word is written whole, so a reader sees
/// a state the writer had finished rather than half of an answer.
///
/// It starts as [`Link::Joining`] because that is the state this chip is in from the moment it
/// starts: the radio is expected to be trying, and `join` replaces this with something more specific
/// within a few lines of returning.
static LINK: AtomicU32 = AtomicU32::new(Link::Joining.to_word());

/// Starts the radio, joins the network, and returns the network stack once it exists.
///
/// The stack is returned immediately rather than once DHCP has produced an address, because the
/// address arrives in the `report_address` task and the caller has no reason to wait for it:
/// everything this function starts runs in its own task. `None` means there was no network to join,
/// either because no credentials were compiled in or because the radio refused to start; in both
/// cases the rest of the firmware is unaffected, and what went wrong is on the state line.
///
/// # Panics
///
/// If the executor has no room left for another task. All three are allocated once, at boot, so
/// this is a fact about the size of the task pool rather than something that can happen later.
pub fn join(spawner: Spawner, device: WIFI<'static>) -> Option<Stack<'static>> {
    let (ssid, password) = match credentials() {
        Ok(credentials) => credentials,
        Err(link) => {
            // Published rather than only logged, because the greeting reads this long after the
            // boot that caused it, and a reader working backwards from a missing address is being
            // asked to guess.
            publish(link);

            return None;
        }
    };

    info!("joining {}", ssid);

    // The interface is a singleton, so it has to be claimed before the controller starts and is
    // handed to the network stack below.
    let interface = Interface::station();

    let station = Config::Station(
        StationConfig::default()
            .with_ssid(ssid)
            .with_authentication(AuthenticationMethodConfig::Wpa2Personal(password)),
    );

    let controller = match WifiController::new(
        device,
        ControllerConfig::default().with_initial_config(station),
    ) {
        Ok(controller) => controller,
        Err(e) => {
            error!("the radio did not start: {:?}", e);

            publish(Link::NoRadio);

            return None;
        }
    };

    // The stack needs a seed for its own bookkeeping; the hardware RNG is the one thing here that
    // is neither the radio nor the network.
    let rng = Rng::new();
    let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());

    let (stack, runner) = embassy_net::new(
        interface,
        embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default()),
        static_cell::make_static!(StackResources::<SOCKETS>::new()),
        seed,
    );

    spawner.spawn(keep_joined(controller).expect("keep_joined is a task"));
    spawner.spawn(run_stack(runner).expect("run_stack is a task"));
    spawner.spawn(report_address(stack).expect("report_address is a task"));

    Some(stack)
}

/// What the radio is doing right now, as of the last thing this file published.
///
/// Never waits and never fails, which is what lets the greeting call it twice a second without
/// knowing anything about the radio. Six of the seven states it can be in were only ever decided in
/// this file's own task: what the hardware said is here, and so is what to do about it.
#[must_use]
pub fn link() -> Link {
    Link::from_word(LINK.load(Ordering::Acquire))
}

/// Records what the radio is doing, for [`link`] to read.
///
/// One whole word, so there is no order to get right between two halves of an answer. Release and
/// acquire rather than `Relaxed` because the word is written by the radio's task and read by the
/// greeting's, and the chip has one core: this is the ordering that makes the write visible.
fn publish(link: Link) {
    LINK.store(link.to_word(), Ordering::Release);
}

/// The address in a stack's IPv4 configuration, in the form the firmware prints.
///
/// In one place because the greeting in `src/bin/main.rs` and the report below both print an
/// address, and two places turning one configuration into an [`Address`] are two places that can
/// print two different ones.
#[must_use]
pub fn address(config: &StaticConfigV4) -> Address {
    // `embassy-net` speaks in `smoltcp`'s address types, which since `smoltcp` 0.13 are the ones in
    // `core::net` — the same types everything else on the chip uses, and the ones `defmt` prints.
    Address {
        ip: config.address.address(),
        prefix_len: config.address.prefix_len(),
    }
}

/// The credentials to join with, or the [`Link`] that says why there are none to use.
///
/// [`SSID`] is what says whether there are credentials at all: a build with no `WIFI_SSID`
/// compiled in has no network to join, which is the normal state of the gate and of CI. The two
/// answers are different states because they are different problems — one is a build with nothing to
/// try, the other is a build with something in it the radio will not accept — and a reader who
/// cannot tell them apart will look in the wrong place.
fn credentials() -> Result<(Ssid, Password), Link> {
    let ssid = SSID.ok_or(Link::NoNetwork)?;
    let password = PASSWORD.unwrap_or_default();

    Ok((
        usable("SSID", Ssid::try_from(ssid))?,
        usable("password", Password::try_from(password))?,
    ))
}

/// Reports a credential the radio cannot use, and gives up on it.
///
/// Both values are compiled in, so this is a mistake in the environment the firmware was built from
/// rather than anything that can be recovered from at runtime. The [`Link`] it answers with is the
/// one the state line prints and this line is about, so the two cannot disagree about what went
/// wrong.
fn usable<T>(what: &str, parsed: Result<T, WifiError>) -> Result<T, Link> {
    parsed.map_err(|e| {
        error!("the {} is not usable: {:?}", what, e);

        Link::UnusableCredential
    })
}

/// Keeps the station joined, and joins again when the link drops.
///
/// A failed attempt is followed by a scan. "Could not join" on its own does not distinguish a
/// broken radio from a wrong password from a network that is simply not there, and those three
/// want three different fixes.
#[embassy_executor::task]
async fn keep_joined(mut controller: WifiController<'static>) {
    loop {
        // Published before the attempt rather than after it: an attempt takes seconds, and a state
        // line that still says "joined" for as long as one takes is wrong for as long as one takes.
        publish(Link::Joining);

        match controller.connect_async().await {
            Ok(joined) => {
                info!("joined {}", joined.ssid);

                publish(Link::Joined);

                // This is where the time until the link drops is spent. After a failed attempt there
                // is nothing to wait for: the controller knows it is not connected, and refuses to
                // wait for a disconnection event that will never arrive.
                if let Err(e) = controller.wait_for_disconnect_async().await {
                    error!("lost the link: {:?}", e);
                }

                // The link is down from here until the next attempt starts, and the retry below would
                // otherwise spend five seconds of it claiming that this chip is on a network.
                publish(Link::Joining);
            }
            Err(e) => {
                report(&e);
                list_neighbors(&mut controller).await;
            }
        }

        Timer::after(RETRY).await;
    }
}

/// Says why the last attempt to join did not work, and publishes it.
///
/// A disconnected station is reported by the radio as a numbered reason out of about fifty, which
/// answers "what did the hardware say" and leaves "what do I do about it" to whoever is reading the
/// serial output. This translates the ones that come up in practice and keeps the rest, because a
/// wrong guess is worse than an honest unknown: two of these look alike in the radio's words and want
/// opposite fixes — a refused password is a typo, and a network that stops answering is a station too
/// far away.
///
/// `poc_report` decides the wording and is tested on the host; the radio's own words are still printed
/// underneath for anything this does not name. Published rather than only printed, because the last
/// failure is what the greeting keeps saying for as long as the link is down — which, with the retry
/// above, is most of the time on a network that is not there.
fn report(failure: &ConnectionError) {
    match failure {
        ConnectionError::Failed(info) => {
            let failure = JoinFailure {
                reason: named(info.reason),
                signal: measured(info.rssi),
            };

            publish(Link::Failed(failure));

            error!(
                "could not join {}: {} ({:?})",
                info.ssid,
                defmt::Display2Format(&failure),
                info.reason,
            );
        }
        // Not a join at all, so there is no network name and no signal to report either: whatever the
        // radio said is in the line below, and the state line says only that the attempt did not work
        // and that this firmware has no name for why.
        other => {
            publish(Link::Failed(JoinFailure {
                reason: Reason::Other,
                signal: None,
            }));

            error!("could not join: {:?}", other);
        }
    }
}

/// The cause, in as few words as the radio's fifty reasons allow.
///
/// Everything not named here is [`Reason::Other`]: the enum is `#[non_exhaustive]`, so a reason added
/// upstream lands there rather than failing to build, and this is the place to teach the firmware a
/// new one.
fn named(reason: DisconnectReason) -> Reason {
    match reason {
        // Nothing answered with this name at all: the network is not there, the name is wrong, or the
        // station is out of range. The scan printed after a failure is what tells those apart.
        DisconnectReason::NoAccessPointFound
        | DisconnectReason::NoAccessPointFoundInRssiThreshold
        | DisconnectReason::BeaconTimeout => Reason::NoSuchNetwork,

        // The network was heard and said no. A refused PSK is the usual answer, and a WPA3-only
        // network is the other one worth suspecting before the password.
        DisconnectReason::NoAccessPointFoundWithCompatibleSecurity
        | DisconnectReason::NoAccessPointFoundInAuthmodeThreshold
        | DisconnectReason::AuthenticationFailed
        | DisconnectReason::FourWayHandshakeTimeout
        | DisconnectReason::MicFailure
        | DisconnectReason::IeIn4wayDiffers
        | DisconnectReason::_802_1xAuthenticationFailed
        | DisconnectReason::CipherSuiteRejected
        | DisconnectReason::BadCipherOrAkm => Reason::SecurityRefused,

        // The exchange began and the other end went quiet. From the station's side this is what being
        // too far away looks like: the beacons arrive, the handshake does not.
        DisconnectReason::AuthenticationExpired
        | DisconnectReason::HandshakeTimeout
        | DisconnectReason::Timeout => Reason::NoAnswer,

        // It was up, and then it was not.
        DisconnectReason::DisassociatedDueToInactivity
        | DisconnectReason::AuthenticationLeave
        | DisconnectReason::AssociationLeave
        | DisconnectReason::PeerInitiated
        | DisconnectReason::AccessPointInitiatedDisassociation => Reason::LinkLost,

        // The enum is `#[non_exhaustive]`: a reason this build has never heard of is reported rather
        // than guessed at.
        _ => Reason::Other,
    }
}

/// The signal the radio last measured, or `None` when it reported none.
///
/// The driver fills in `-128` when it has no reading, which is a number that would be printed as
/// though it had been measured — and a station that appears to have heard the network at -128 dBm is
/// a contradiction worth not printing.
fn measured(rssi: i8) -> Option<i8> {
    (rssi > -128).then_some(rssi)
}

/// Names the access points the radio can hear, and how loudly.
async fn list_neighbors(controller: &mut WifiController<'_>) {
    let config = ScanConfig::default().with_max(NEIGHBORS);

    match controller.scan_async(&config).await {
        Ok(neighbors) => {
            for neighbor in neighbors {
                info!(
                    "  {} at {} dBm, channel {}",
                    neighbor.ssid, neighbor.signal_strength, neighbor.channel
                );
            }
        }
        Err(e) => error!("the scan failed: {:?}", e),
    }
}

/// Drives the TCP/IP stack. This is the task that has to be running for DHCP to happen at all.
#[embassy_executor::task]
async fn run_stack(mut runner: Runner<'static, Interface>) {
    runner.run().await;
}

/// Waits for DHCP to produce an address, and prints it.
#[embassy_executor::task]
async fn report_address(stack: Stack<'static>) {
    stack.wait_config_up().await;

    if let Some(config) = stack.config_v4() {
        // Printed by `address`, the same function the greeting reads the address with, so this task
        // and the state line cannot end up naming two different networks.
        info!("address {}", defmt::Display2Format(&address(&config)));

        if let Some(gateway) = config.gateway {
            info!("gateway {}", gateway);
        }

        for server in &config.dns_servers {
            info!("DNS {}", server);
        }
    } else {
        error!("DHCP is up but there is no address");
    }
}
