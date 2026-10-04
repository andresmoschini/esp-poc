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

use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig, DisconnectReason,
    Interface, Password, Ssid, WifiController, WifiError, scan::ScanConfig, sta::StationConfig,
};
use poc_report::{Address, Failure, Reason};

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

/// Starts the radio, joins the network, and returns the network stack once it exists.
///
/// The stack is returned immediately rather than once DHCP has produced an address, because the
/// address arrives in the `report_address` task and the caller has no reason to wait for it:
/// everything this function starts runs in its own task. `None` means there was no network to join,
/// either because no credentials were compiled in or because the radio refused to start; in both
/// cases the rest of the firmware is unaffected.
///
/// # Panics
///
/// If the executor has no room left for another task. All three are allocated once, at boot, so
/// this is a fact about the size of the task pool rather than something that can happen later.
pub fn join(spawner: Spawner, device: WIFI<'static>) -> Option<Stack<'static>> {
    let (ssid, password) = credentials()?;

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

/// The credentials to join with, or `None` if there are none to use.
///
/// [`SSID`] is what says whether there are credentials at all: a build with no `WIFI_SSID`
/// compiled in has no network to join, which is the normal state of the gate and of CI.
fn credentials() -> Option<(Ssid, Password)> {
    let ssid = SSID?;
    let password = PASSWORD.unwrap_or_default();

    Some((
        usable("SSID", Ssid::try_from(ssid))?,
        usable("password", Password::try_from(password))?,
    ))
}

/// Reports a credential the radio cannot use, and gives up on it.
///
/// Both values are compiled in, so this is a mistake in the environment the firmware was built
/// from rather than anything that can be recovered from at runtime.
fn usable<T>(what: &str, parsed: Result<T, WifiError>) -> Option<T> {
    match parsed {
        Ok(value) => Some(value),
        Err(e) => {
            error!("the {} is not usable: {:?}", what, e);
            None
        }
    }
}

/// Keeps the station joined, and joins again when the link drops.
///
/// A failed attempt is followed by a scan. "Could not join" on its own does not distinguish a
/// broken radio from a wrong password from a network that is simply not there, and those three
/// want three different fixes.
#[embassy_executor::task]
async fn keep_joined(mut controller: WifiController<'static>) {
    loop {
        match controller.connect_async().await {
            Ok(joined) => {
                info!("joined {}", joined.ssid);

                // This is where the time until the link drops is spent. After a failed attempt there
                // is nothing to wait for: the controller knows it is not connected, and refuses to
                // wait for a disconnection event that will never arrive.
                if let Err(e) = controller.wait_for_disconnect_async().await {
                    error!("lost the link: {:?}", e);
                }
            }
            Err(e) => {
                report(&e);
                list_neighbors(&mut controller).await;
            }
        }

        Timer::after(RETRY).await;
    }
}

/// Says why the last attempt to join did not work.
///
/// A disconnected station is reported by the radio as a numbered reason out of about fifty, which
/// answers "what did the hardware say" and leaves "what do I do about it" to whoever is reading the
/// serial output. This translates the ones that come up in practice and keeps the rest, because a
/// wrong guess is worse than an honest unknown: two of these look alike in the radio's words and want
/// opposite fixes — a refused password is a typo, and a network that stops answering is a station too
/// far away.
///
/// `poc_report` decides the wording and is tested on the host; the radio's own words are still printed
/// underneath for anything this does not name.
fn report(error: &ConnectionError) {
    match error {
        ConnectionError::Failed(info) => error!(
            "could not join {}: {} ({:?})",
            info.ssid,
            defmt::Display2Format(&Failure {
                reason: named(info.reason),
                signal: measured(info.rssi),
            }),
            info.reason,
        ),
        other => error!("could not join: {:?}", other),
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

    // `embassy-net` speaks in `smoltcp`'s address types, which since `smoltcp` 0.13 are the ones in
    // `core::net` — the same types everything else on the chip uses, and the ones `defmt` prints.
    if let Some(config) = stack.config_v4() {
        info!(
            "address {}",
            defmt::Display2Format(&Address {
                ip: config.address.address(),
                prefix_len: config.address.prefix_len(),
            })
        );

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
