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
//! `join` returns is the handle to the network stack, which is what a later step needs — an SNTP
//! request, say — and which is `Copy`, so a task can take its own copy of it.
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
    AuthenticationMethodConfig, Config, ControllerConfig, Interface, Password, Ssid,
    WifiController, WifiError, scan::ScanConfig, sta::StationConfig,
};

/// The SSID of the network to join, read from the environment when this was compiled.
const SSID: Option<&str> = option_env!("WIFI_SSID");

/// The password of that network, likewise. Empty for an open network.
const PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

/// How long to wait between attempts to join, whether the last one failed or succeeded.
const RETRY: Duration = Duration::from_secs(5);

/// How many access points to name when a connection attempt fails.
const NEIGHBORS: usize = 10;

/// Sockets the network stack is sized for. The proof of concept opens none — DHCP does not use
/// one — but the stack's storage is static and has to be sized for something.
const SOCKETS: usize = 3;

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
                error!("could not join: {:?}", e);
                list_neighbors(&mut controller).await;
            }
        }

        Timer::after(RETRY).await;
    }
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
            "address {}/{}",
            config.address.address(),
            config.address.prefix_len()
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
