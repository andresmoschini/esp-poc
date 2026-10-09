//! Join a Wi-Fi network, take an address over DHCP, and publish what the radio is doing.
//!
//! Wi-Fi on this chip is three pieces, each a precondition of the next: `esp-radio` drives the radio
//! and claims a peripheral, which is how `esp-hal` says only one task may drive it; `esp-rtos` is
//! the preemptive scheduler that driver will not start without, which is why [`join`] is called
//! after `esp_rtos::start`; and `embassy-net` turns a joined network into an address. Nothing here
//! waits for the network — [`join`] starts three tasks and returns the stack, which is what
//! `src/ntp.rs` and `src/report.rs` run on.
//!
//! The radio runs in a task and the greeting in another, so nothing here can return the state of the
//! link to it. This file publishes that as a [`Link`] — what it is doing, and the driver's own reason
//! when it stopped — behind a lock rather than as one word, because a value split across several
//! words is a value whose parts can be read at different moments.
//!
//! The SSID and password are compiled in from `WIFI_SSID` and `WIFI_PASSWORD`, read with
//! `option_env!` rather than `env!` so a build with neither still compiles — which is what the gate
//! and CI are. There, [`join`] says so on the state line and returns `None`.

use core::cell::RefCell;

use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_net::{Runner, Stack, StackResources, StaticConfigV4};
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use embassy_time::{Duration, Timer};
use esp_hal::peripherals::WIFI;
use esp_hal::rng::Rng;
use esp_radio::wifi::{
    AuthenticationMethodConfig, Config, ConnectionError, ControllerConfig, DisconnectReason,
    Interface, Password, Ssid, WifiController, WifiError, scan::ScanConfig, sta::StationConfig,
};
use poc_domain::Address;

const SSID: Option<&str> = option_env!("WIFI_SSID");

/// The password of that network, likewise. Empty for an open network.
const PASSWORD: Option<&str> = option_env!("WIFI_PASSWORD");

/// How long to wait between attempts to join, whether the last one failed or succeeded.
const RETRY: Duration = Duration::from_secs(5);

const NEIGHBORS: usize = 10;

/// Sockets the network stack is sized for.
///
/// Four of them are taken before anything here asks for one: DHCP takes a socket when the stack is
/// first configured, the DNS resolver takes one when the stack is built, the SNTP client in
/// `src/ntp.rs` takes the third, and the HTTP reporter in `src/report.rs` takes the fourth. The fifth
/// is the point — `smoltcp`'s socket set is a fixed-size array that panics when it is full rather than
/// refusing the socket that does not fit, so this is the number that decides whether the next thing
/// to want a socket works at all.
const SOCKETS: usize = 5;

/// What the radio is doing, published for [`link`] to read.
///
/// A lock around the [`Link`] rather than one atomic holding a word for it: on a chip with one
/// core the lock is a brief critical section, taken twice a second by the greeting and written on
/// every join attempt, and what it holds is the value itself rather than an encoding of it. The
/// `RefCell` is what makes the write safe without an `unsafe`: nothing locks again inside a lock
/// closure and no interrupt handler touches this static, so the borrow cannot fail — and if that
/// ever stops being true it panics rather than corrupting.
///
/// It starts as [`Link::Joining`] because that is the state this chip is in from the moment it
/// starts: the radio is expected to be trying, and `join` replaces this with something more specific
/// within a few lines of returning.
static LINK: Mutex<CriticalSectionRawMutex, RefCell<Link>> =
    Mutex::new(RefCell::new(Link::Joining));

/// What the radio is doing, and — when it is not doing what it should — why.
///
/// A state rather than a sequence of log lines because a log is read long after the event. A line
/// saying it could not join helps only whoever is watching when it happens; a line that still says
/// so twice a second is what someone reading the log from the top finds, and to them the two are the
/// same thing.
///
/// The states are separate rather than one "not connected" because each wants a different thing done
/// about it: fill in the credentials, fix a credential, replace the board, or go and look for the
/// network and how loudly it can be heard from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    /// There was never a network to join, because no `SSID` was compiled in.
    ///
    /// The normal state of the gate and of CI, and the state a build made before `.cargo/local.toml`
    /// was filled in is in. Nothing is wrong with the hardware, which is what makes this worth
    /// distinguishing from every other state here.
    NoNetwork,

    /// There was a network to join and the radio would not take one of its credentials.
    ///
    /// Both values are compiled in, so this is a mistake in the file the firmware was built from
    /// rather than anything that can happen on a board.
    UnusableCredential,

    /// The radio would not start at all, which is neither a network problem nor a credentials one.
    NoRadio,

    /// An attempt is in progress, or one is due.
    Joining,

    /// The station is on the network.
    Joined,

    /// The last attempt did not work, and this is what the radio said.
    Failed(JoinFailure),
}

impl core::fmt::Display for Link {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            // The same three words on the three states with no network in them, because what they
            // have in common is what a reader has to act on first: there is nothing to wait for.
            Self::NoNetwork => f.write_str("nothing to join: no credentials were compiled in"),
            Self::UnusableCredential => f.write_str(
                "nothing to join: a compiled-in credential is not one the radio can use",
            ),
            Self::NoRadio => f.write_str("nothing to join: the radio did not start"),
            Self::Joining => f.write_str("joining"),
            Self::Joined => f.write_str("joined"),
            Self::Failed(failure) => write!(f, "not joined: {failure}"),
        }
    }
}

/// A join that failed, with what the radio said and the signal it last measured.
///
/// The reason is the driver's own [`DisconnectReason`], carried as-is rather than grouped: any
/// grouping is a claim about what to do next, and a wrong claim sends whoever is reading the serial
/// output after the wrong problem. The signal is what tells the two failures that look identical in
/// the radio's own words apart — a refused password and a station too far away both arrive as
/// "could not join".
///
/// The reason renders as the driver's own variant name, which is a Rust identifier: no quotes, no
/// backslash, no controls, so it cannot end the JSON string the state line is reported in — by
/// construction rather than by test, since this file cannot be compiled for a host at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinFailure {
    /// What the radio reported.
    pub reason: DisconnectReason,

    /// How strong the signal was, in dBm, when the radio measured one.
    pub signal: Option<i8>,
}

impl core::fmt::Display for JoinFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.signal {
            Some(dbm) => write!(f, "{:?} (signal {dbm} dBm)", self.reason),
            None => write!(f, "{:?}", self.reason),
        }
    }
}

/// Starts the radio, joins the network, and returns the network stack once it exists.
///
/// `None` means there was no network to join: the radio did not start, or there were no credentials
/// compiled in. Either way the rest of the firmware is unaffected, and what went wrong is on the
/// state line rather than only in the log — the greeting reads this long after the boot that caused
/// it, and a reader working backwards from a missing address is being asked to guess.
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
/// knowing anything about the radio. Every state it can be in was only ever decided in this file's
/// own task: what the hardware said is here, and so is what to do about it.
#[must_use]
pub fn link() -> Link {
    LINK.lock(|link| *link.borrow())
}

/// Records what the radio is doing, for [`link`] to read.
///
/// The whole value under one lock, so a reader sees a state the writer had finished rather than
/// half of an answer.
fn publish(link: Link) {
    LINK.lock(|slot| *slot.borrow_mut() = link);
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
/// serial output: a wrong guess here is worse than an honest unknown, because two failures that look
/// alike in the radio's words want opposite fixes — a refused password is a typo, and a network
/// that stops answering is a station too far away. So this carries the reason as-is rather than
/// grouping it, and the scan printed after a failure is what tells the cases apart.
///
/// Published rather than only printed, because the last failure is what the greeting keeps saying
/// for as long as the link is down — which, with the retry above, is most of the time on a network
/// that is not there.
fn report(failure: &ConnectionError) {
    match failure {
        ConnectionError::Failed(info) => {
            let failure = JoinFailure {
                reason: info.reason,
                signal: measured(info.rssi),
            };

            publish(Link::Failed(failure));

            error!(
                "could not join {}: {}",
                info.ssid,
                defmt::Display2Format(&failure),
            );
        }
        // Not a join at all, so there is no network name and no signal to report either: whatever the
        // radio said is in the line below, and the state line carries the driver's own shrug.
        other => {
            publish(Link::Failed(JoinFailure {
                reason: DisconnectReason::Unspecified,
                signal: None,
            }));

            error!("could not join: {:?}", other);
        }
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
