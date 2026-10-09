//! #### PR #40
//! Device reports and controls for the workers, mixed from two sources.
//! asic-rs (256 Foundation, Apache-2.0) is the dependency: it identifies most
//! SHA-256 makes and firmwares (Antminer, Whatsminer, Avalon, Bitaxe, NerdAxe,
//! Braiins OS, Vnish, LuxOS, ePIC, Auradine and more) and speaks each one's
//! API. Pickaxe's own CGMiner and Bitaxe code in `device_api` is the
//! extension, used where asic-rs has no answer or a different one: devices it
//! cannot identify, the windowed hash rate a CGMiner device's own app shows
//! (asic-rs reads Avalon's 1-minute rate; the app shows 5 minutes), Avalon's
//! hottest reading, Avalon Nano power (asic-rs reads the Nano's input
//! voltage as watts), and Avalon work levels, which asic-rs's power limit in
//! watts cannot express.
//!
//! Only devices on the local network that connected to this server are asked.
//! Pool settings (their worker names are often payout addresses), MAC
//! addresses, serial numbers and host names are never collected.

use super::device_api::{self, DeviceAction, DeviceReport, OwnApi, PowerMode};
use asic_rs::{
    core::{
        config::{fan::FanConfig, tuning::TuningConfig},
        data::{
            board::BoardData,
            collector::DataField,
            fan::FanData,
            hashrate::HashRateUnit,
            miner::{MinerData, MiningMode, TuningTarget},
        },
        traits::miner::Miner,
    },
    MinerFactory,
};
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// Time allowed to identify a device's make and firmware.
const IDENTIFY: Duration = Duration::from_secs(10);
/// Time allowed for one device's report, or for one action.
const ANSWER: Duration = Duration::from_secs(10);
/// How long a device asic-rs could not identify waits before another try;
/// Pickaxe's own reader answers for it meanwhile.
const RETRY: Duration = Duration::from_secs(300);
/// Devices asked at the same time, so a farm is polled in one pass.
const PARALLEL: usize = 32;
/// Data never collected: identities, pool settings, and per-chip detail.
const NOT_COLLECTED: [DataField; 10] = [
    DataField::Mac,
    DataField::SerialNumber,
    DataField::Hostname,
    DataField::Pools,
    DataField::Chips,
    DataField::Messages,
    DataField::DevFeeConnected,
    DataField::BestShare,
    DataField::SessionBestShare,
    DataField::TuningCapabilities,
];

pub struct Fleet {
    // Absent only if the runtime could not start; Pickaxe's own reader and
    // controls then answer alone.
    runtime: Option<tokio::runtime::Runtime>,
    shared: Arc<Shared>,
}

struct Shared {
    factory: MinerFactory,
    known: Mutex<HashMap<IpAddr, Known>>,
    /// #### PR #42
    /// What: the address of the device the Device panel opened last; the
    /// poller's passes keep what is known about it.
    /// Why: each pass forgets every device that is not connected, and the
    /// panel opens on offline rows too. A confirmed action then found no
    /// identified device: Pause or Blink failed, and Restart on a make only
    /// asic-rs speaks to took Pickaxe's own Canaan and Bitaxe commands.
    /// Look here if: an action on an offline row fails or takes another path
    /// than the panel listed, or a farm's churn grows the cache.
    held: Mutex<Option<IpAddr>>,
    /// #### PR #42: what each Avalon listed in its `ascset 0,help` reply,
    /// read when its Device panel identifies it; `None` when it listed
    /// nothing readable.
    avalon_help: Mutex<HashMap<IpAddr, Option<Vec<String>>>>,
}

#[derive(Clone)]
enum Known {
    Miner(Arc<dyn Miner>),
    Unknown(Instant),
}

impl Default for Fleet {
    fn default() -> Self {
        Self::new()
    }
}

impl Fleet {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("pickaxe-devices")
            .enable_all()
            .build()
            .ok();
        Self {
            runtime,
            shared: Arc::new(Shared {
                factory: MinerFactory::new().with_identification_timeout(IDENTIFY),
                known: Mutex::new(HashMap::new()),
                held: Mutex::new(None),
                avalon_help: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Reports for these devices, asked in parallel, each within its time
    /// limit. Returns early, with the reports so far, once `stop` is set.
    pub fn poll(
        &self,
        devices: &[(u64, IpAddr)],
        stop: &AtomicBool,
    ) -> Vec<(u64, Option<DeviceReport>)> {
        self.shared.keep_only(devices);
        let Some(runtime) = &self.runtime else {
            return devices
                .iter()
                .map(|&(id, ip)| (id, device_api::poll(ip)))
                .collect();
        };
        runtime.block_on(async {
            let permits = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
            let mut tasks = tokio::task::JoinSet::new();
            for &(id, ip) in devices {
                let shared = Arc::clone(&self.shared);
                let permits = Arc::clone(&permits);
                tasks.spawn(async move {
                    let _permit = permits.acquire_owned().await;
                    (id, shared.report(ip).await)
                });
            }
            let mut reports = Vec::new();
            while !tasks.is_empty() {
                if stop.load(Ordering::Relaxed) {
                    tasks.abort_all();
                    break;
                }
                // Waiting on the next report is cancel-safe.
                if let Ok(Some(Ok(report))) =
                    tokio::time::timeout(Duration::from_millis(100), tasks.join_next()).await
                {
                    reports.push(report);
                }
            }
            reports
        })
    }

    /// #### PR #42
    /// What: what this device offers on its Device panel, from what is
    /// already known about it; no device is asked. Nothing for an address
    /// that is not on the local network.
    /// Why: the panel is drawn every half second and must never wait on a
    /// device; `identify_now` asks it, on a background thread.
    /// Look here if: the panel lists the wrong actions for a device.
    pub fn controls(&self, ip: IpAddr) -> DeviceControls {
        if !device_api::queryable(ip) {
            return DeviceControls::default();
        }
        let help = self.shared.avalon_help(ip);
        controls_of(self.shared.miner(ip).as_deref(), help.as_deref())
    }

    /// #### PR #42
    /// What: identifies the device for its Device panel, within `IDENTIFY`
    /// (10 seconds), and returns what it offers, from that identification
    /// itself. `fresh` asks the device again even if asic-rs knows it (or
    /// could not identify it lately). The address is held, so the poller
    /// keeps what was found for the panel's actions. It blocks, so call it
    /// from a background thread, never the screen's.
    /// Why: the panel opens on offline rows too, which the poller no longer
    /// asks; their make may not be known, and their address may since have
    /// passed to another device, which only a fresh look shows.
    /// Look here if: the panel stays at "Identifying the device", the screen
    /// freezes when the panel opens, or an offline row's panel shows the
    /// make the device had before.
    pub fn identify_now(&self, ip: IpAddr, fresh: bool) -> DeviceControls {
        if !device_api::queryable(ip) {
            return DeviceControls::default();
        }
        self.shared.hold(ip);
        let miner = self.runtime.as_ref().and_then(|runtime| {
            // The time limit is made inside the runtime (see `control`).
            runtime
                .block_on(async {
                    tokio::time::timeout(IDENTIFY, self.shared.identify(ip, fresh)).await
                })
                .ok()
                .flatten()
        });
        // #### PR #42: an Avalon lists the settings it accepts (read-only).
        let help = miner
            .as_deref()
            .filter(|miner| is_avalon(*miner))
            .and_then(|_| {
                let help = device_api::avalon_options(ip);
                self.shared.remember_help(ip, help.clone());
                help
            });
        controls_of(miner.as_deref(), help.as_deref())
    }

    /// Runs one confirmed action and describes the device's answer: through
    /// asic-rs where it supports the action, otherwise Pickaxe's own code.
    pub fn control(&self, ip: IpAddr, action: DeviceAction) -> Result<String, String> {
        if !device_api::queryable(ip) {
            return Err("only devices on the local network can be controlled".into());
        }
        match (self.through_asic_rs(ip, action), &self.runtime) {
            (Some(miner), Some(runtime)) => run_within_answer(runtime, &*miner, action),
            // #### PR #42: fan and power settings asic-rs does not send for
            // this device go to Pickaxe's own Avalon or AxeOS commands.
            _ => match (
                is_setting(action),
                self.shared.miner(ip).as_deref().and_then(own_api),
            ) {
                (true, Some(api)) => device_api::setting(ip, api, action),
                (true, None) => Err("this device does not offer that action".into()),
                (false, _) => device_api::control(ip, action),
            }
            .map(|reply| format!("the device replied \"{reply}\"")),
        }
    }

    /// The identified device an action goes to through asic-rs, when asic-rs
    /// supports that action for it; Pickaxe's own code sends the rest.
    fn through_asic_rs(&self, ip: IpAddr, action: DeviceAction) -> Option<Arc<dyn Miner>> {
        self.shared
            .miner(ip)
            .filter(|miner| supports(&**miner, action))
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        // A device still answering cannot hold up shutdown.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

impl Shared {
    /// Forgets devices that left, so a farm's churn cannot grow the cache;
    /// the device the Device panel holds is kept (PR #42, see `held`).
    fn keep_only(&self, devices: &[(u64, IpAddr)]) {
        let held = self.held.lock().ok().and_then(|held| *held);
        if let Ok(mut known) = self.known.lock() {
            known.retain(|ip, _| {
                held == Some(*ip) || devices.iter().any(|(_, device)| device == ip)
            });
        }
        if let Ok(mut help) = self.avalon_help.lock() {
            help.retain(|ip, _| {
                held == Some(*ip) || devices.iter().any(|(_, device)| device == ip)
            });
        }
    }

    /// #### PR #42: what an Avalon listed in its `ascset 0,help` reply;
    /// `None` when it has not been asked or listed nothing readable.
    fn avalon_help(&self, ip: IpAddr) -> Option<Vec<String>> {
        self.avalon_help.lock().ok()?.get(&ip).cloned().flatten()
    }

    fn remember_help(&self, ip: IpAddr, help: Option<Vec<String>>) {
        if let Ok(mut known) = self.avalon_help.lock() {
            known.insert(ip, help);
        }
    }

    /// #### PR #42: the Device panel's device, kept through polls.
    fn hold(&self, ip: IpAddr) {
        if let Ok(mut held) = self.held.lock() {
            *held = Some(ip);
        }
    }

    /// The identified miner at this address, if asic-rs knows it.
    fn miner(&self, ip: IpAddr) -> Option<Arc<dyn Miner>> {
        match self.known.lock().ok()?.get(&ip) {
            Some(Known::Miner(miner)) => Some(Arc::clone(miner)),
            _ => None,
        }
    }

    /// Identifies the device once; one asic-rs could not identify is asked
    /// again only after `RETRY`. (PR #42: `fresh` forgets what is known and
    /// asks now, so nothing older is used if this look times out.)
    async fn identify(&self, ip: IpAddr, fresh: bool) -> Option<Arc<dyn Miner>> {
        let known = {
            let mut known = self.known.lock().ok()?;
            if fresh {
                known.remove(&ip)
            } else {
                known.get(&ip).cloned()
            }
        };
        match known {
            Some(Known::Miner(miner)) if !fresh => return Some(miner),
            Some(Known::Unknown(since)) if !fresh && since.elapsed() < RETRY => return None,
            _ => (),
        }
        let found = tokio::time::timeout(IDENTIFY, self.factory.get_miner(ip)).await;
        let entry = match found {
            Ok(Ok(Some(miner))) => Known::Miner(Arc::from(miner)),
            _ => Known::Unknown(Instant::now()),
        };
        let miner = match &entry {
            Known::Miner(miner) => Some(Arc::clone(miner)),
            Known::Unknown(_) => None,
        };
        if let Ok(mut known) = self.known.lock() {
            known.insert(ip, entry);
        }
        miner
    }

    /// Both sources at once: asic-rs's data and Pickaxe's own reading.
    async fn report(&self, ip: IpAddr) -> Option<DeviceReport> {
        if !device_api::queryable(ip) {
            return None;
        }
        let own = tokio::task::spawn_blocking(move || device_api::poll(ip));
        let data = async {
            let miner = self.identify(ip, false).await?;
            tokio::time::timeout(ANSWER, miner.get_data_filtered(NOT_COLLECTED.to_vec()))
                .await
                .ok()
        };
        let (own, data) = tokio::join!(own, data);
        mix(own.ok().flatten(), data.as_ref().map(from_asic_rs))
    }
}

/// #### PR #42
/// What a device offers on its Device panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceControls {
    /// The firmware asic-rs identified, such as "AvalonMiner Stock"; none
    /// when it has not identified the device.
    pub firmware: Option<String>,
    /// The make and model asic-rs identified, written as a device report's
    /// model is ("Avalonminer AvalonNano3s"), so an offline row's panel can
    /// check that the device answering is the row's; none when asic-rs has
    /// not identified the device.
    pub model: Option<String>,
    /// The one-shot actions, each sent only after the user confirms it.
    pub actions: Vec<DeviceAction>,
    /// #### PR #42: the fan setting offered: automatic, or a percentage in
    /// this range; none when the device offers no fan setting.
    pub fan: Option<FanRange>,
    /// #### PR #42: how power is set, beyond Avalon work-level steps.
    pub power: Option<PowerSetting>,
}

/// #### PR #42: the fan percentages a device accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FanRange {
    pub min: u8,
    pub max: u8,
}

/// #### PR #42: how a device's power is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerSetting {
    /// Named modes: Low, Normal and High.
    Modes,
    /// A limit in watts.
    Watts,
}

/// The firmware asic-rs names ePIC's ("UMC OS"): its power limit is set
/// through its tuning.
const EPIC: &str = "UMC OS";
/// Firmwares whose power is set by named modes through asic-rs.
const NAMED_MODES: [&str; 2] = ["AntMiner Stock", "WhatsMiner Stock"];
/// The target temperature an automatic fan keeps when the device reports
/// none (ePIC and Proto need one).
const AUTO_FAN_TARGET_C: f64 = 65.0;

/// #### PR #42: what decides a device's fan and power settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Capabilities {
    own: Option<OwnApi>,
    firmware: String,
    fan_config: bool,
    power_limit: bool,
    tuning_config: bool,
}

impl Capabilities {
    fn of(miner: &dyn Miner) -> Self {
        Self {
            own: own_api(miner),
            firmware: miner.get_device_info().firmware,
            fan_config: miner.supports_fan_config(),
            power_limit: miner.supports_set_power_limit(),
            tuning_config: miner.supports_tuning_config(),
        }
    }
}

// #### PR #42: fan and power settings on the Device panel
// What: the settings a device offers. Avalons: Canaan's fan (15% to 100%)
// and work modes, each when the device's `ascset 0,help` lists it (`fan-spd`,
// `workmode`) or lists nothing readable, so the device has the last word;
// their power otherwise stays in work-level steps. Bitaxe and NerdAxe:
// AxeOS's fan. Others: what asic-rs sets for the firmware: a fan (stock
// Antminer, ePIC, Proto), a power limit in watts (where asic-rs sets one,
// and ePIC through its tuning), or named modes (stock Antminer, WhatsMiner).
// Why: the panel offers only what will be sent. asic-rs sends an Avalon's
// "power limit" as watts text in place of a work level, so Avalon power never
// goes through it.
// Look here if: a panel misses a fan or power setting the device has, or
// offers one it refuses.
fn settings_offered(
    caps: &Capabilities,
    help: Option<&[String]>,
) -> (Option<FanRange>, Option<PowerSetting>) {
    let lists = |option: &str| help.is_none_or(|help| help.iter().any(|word| word == option));
    let any_fan = FanRange { min: 0, max: 100 };
    match caps.own {
        Some(OwnApi::Avalon) => (
            lists("fan-spd").then_some(FanRange {
                min: device_api::AVALON_FAN.0,
                max: device_api::AVALON_FAN.1,
            }),
            lists("workmode").then_some(PowerSetting::Modes),
        ),
        Some(OwnApi::AxeOs) => (Some(any_fan), None),
        None => {
            let power = if caps.power_limit || (caps.firmware == EPIC && caps.tuning_config) {
                Some(PowerSetting::Watts)
            } else if caps.tuning_config && NAMED_MODES.contains(&caps.firmware.as_str()) {
                Some(PowerSetting::Modes)
            } else {
                None
            };
            (caps.fan_config.then_some(any_fan), power)
        }
    }
}

/// Pickaxe's own API for a device's fan and power, if any.
fn own_api(miner: &dyn Miner) -> Option<OwnApi> {
    if is_avalon(miner) {
        return Some(OwnApi::Avalon);
    }
    let firmware = miner.get_device_info().firmware;
    (firmware == "Bitaxe Stock" || firmware == "Nerdaxe Stock").then_some(OwnApi::AxeOs)
}

/// Whether the action is a fan or power setting rather than a one-shot.
fn is_setting(action: DeviceAction) -> bool {
    matches!(
        action,
        DeviceAction::FanAuto
            | DeviceAction::FanPercent(_)
            | DeviceAction::PowerWatts(_)
            | DeviceAction::PowerMode(_)
    )
}
// #### end PR #42 ####

/// What a device offers: what asic-rs supports for its make and firmware,
/// plus Avalon work levels; Pickaxe's own actions for a device asic-rs has
/// not identified.
fn controls_of(miner: Option<&dyn Miner>, help: Option<&[String]>) -> DeviceControls {
    let Some(miner) = miner else {
        return DeviceControls {
            actions: DeviceAction::OWN.to_vec(),
            ..DeviceControls::default()
        };
    };
    let mut actions = vec![DeviceAction::Restart];
    actions.extend(
        [
            DeviceAction::Pause,
            DeviceAction::Resume,
            DeviceAction::LocateOn,
            DeviceAction::LocateOff,
        ]
        .into_iter()
        .filter(|action| supports(miner, *action)),
    );
    if is_avalon(miner) {
        actions.extend([DeviceAction::LowerPower, DeviceAction::RaisePower]);
    }
    let info = miner.get_device_info();
    let (fan, power) = settings_offered(&Capabilities::of(miner), help);
    DeviceControls {
        // The same text `from_asic_rs` gives a report's model.
        model: Some(format!("{} {}", info.make, info.model)),
        firmware: Some(info.firmware),
        actions,
        fan,
        power,
    }
}

fn supports(miner: &dyn Miner, action: DeviceAction) -> bool {
    match action {
        // #### PR #42
        // What: Restart on any Avalon goes to Pickaxe's own Canaan reboot
        // (`ascset 0,reboot,0`, see `device_api::control`), not to asic-rs.
        // Why: asic-rs's Avalon restart sends cgminer's `restart`, which
        // restarts the mining program, not the device, and asic-rs's Avalon
        // Home Q does not offer it at all; Canaan documents
        // `ascset 0,reboot,N` as the device reboot.
        // Look here if: Restart on an Avalon does not reboot it, or a newer
        // asic-rs reboots Avalons itself.
        DeviceAction::Restart => !is_avalon(miner) && miner.supports_restart(),
        DeviceAction::Pause => miner.supports_pause(),
        DeviceAction::Resume => miner.supports_resume(),
        DeviceAction::LocateOn | DeviceAction::LocateOff => miner.supports_set_fault_light(),
        // Work levels are Pickaxe's own Canaan commands.
        DeviceAction::LowerPower | DeviceAction::RaisePower => false,
        // #### PR #42: fan and power settings asic-rs sets for this firmware
        // (see `settings_offered`); Avalons, Bitaxes and NerdAxes use
        // Pickaxe's own commands.
        DeviceAction::FanAuto | DeviceAction::FanPercent(_) => {
            own_api(miner).is_none() && miner.supports_fan_config()
        }
        DeviceAction::PowerWatts(_) => {
            own_api(miner).is_none()
                && (miner.supports_set_power_limit()
                    || (miner.get_device_info().firmware == EPIC && miner.supports_tuning_config()))
        }
        DeviceAction::PowerMode(_) => {
            own_api(miner).is_none()
                && miner.supports_tuning_config()
                && NAMED_MODES.contains(&miner.get_device_info().firmware.as_str())
        }
    }
}

fn is_avalon(miner: &dyn Miner) -> bool {
    miner
        .get_device_info()
        .make
        .to_ascii_lowercase()
        .starts_with("avalon")
}

/// #### PR #42
/// What: runs one action through asic-rs within `ANSWER`, with the time
/// limit made inside the runtime, in the async block it runs.
/// Why: tokio's timer panics when it is made outside a runtime ("there is no
/// reactor running"), as `block_on(timeout(..))` in `control` did: every
/// action sent through asic-rs ended the panel's background thread, and the
/// panel kept showing "Sending".
/// Look here if: an action sent through asic-rs never finishes.
fn run_within_answer(
    runtime: &tokio::runtime::Runtime,
    miner: &dyn Miner,
    action: DeviceAction,
) -> Result<String, String> {
    runtime
        .block_on(async { tokio::time::timeout(ANSWER, run(miner, action)).await })
        .map_err(|_| "the device did not answer in time".to_owned())?
}

async fn run(miner: &dyn Miner, action: DeviceAction) -> Result<String, String> {
    let accepted = match action {
        DeviceAction::Restart => miner.restart().await,
        DeviceAction::Pause => miner.pause(None).await,
        DeviceAction::Resume => miner.resume(None).await,
        DeviceAction::LocateOn => miner.set_fault_light(true).await,
        DeviceAction::LocateOff => miner.set_fault_light(false).await,
        DeviceAction::LowerPower | DeviceAction::RaisePower => {
            return Err("this action is not sent through asic-rs".into())
        }
        // #### PR #42: fan and power settings through asic-rs.
        DeviceAction::FanAuto => {
            // Keep the device's own target temperature when it has one.
            let target = match miner.get_fan_config().await {
                Ok(FanConfig::Auto { target_temp, .. }) => target_temp,
                _ => AUTO_FAN_TARGET_C,
            };
            miner.set_fan_config(FanConfig::auto(target, None)).await
        }
        DeviceAction::FanPercent(percent) => {
            miner
                .set_fan_config(FanConfig::manual(u64::from(percent)))
                .await
        }
        DeviceAction::PowerWatts(watts) => {
            let target = TuningTarget::from_watts(f64::from(watts));
            if miner.get_device_info().firmware == EPIC {
                miner
                    .set_tuning_config(TuningConfig::new(target), None)
                    .await
            } else {
                let TuningTarget::Power(limit) = target else {
                    return Err("this action is not sent through asic-rs".into());
                };
                miner.set_power_limit(limit).await
            }
        }
        DeviceAction::PowerMode(mode) => {
            let mode = match mode {
                PowerMode::Low => MiningMode::Low,
                PowerMode::Normal => MiningMode::Normal,
                PowerMode::High => MiningMode::High,
            };
            miner
                .set_tuning_config(TuningConfig::new(TuningTarget::MiningMode(mode)), None)
                .await
        }
    };
    match accepted {
        Ok(true) => Ok("the device accepted it".into()),
        Ok(false) => Err("the device did not accept it".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// asic-rs's data as a report: hash rate, hottest reading, fans, model,
/// firmware and power.
fn from_asic_rs(data: &MinerData) -> DeviceReport {
    DeviceReport {
        hashrate: data
            .hashrate
            .clone()
            .map(|rate| rate.as_unit(HashRateUnit::Hash).value)
            .filter(|rate| rate.is_finite() && *rate >= 0.0),
        temperature_c: hottest(&data.hashboards)
            .or_else(|| data.average_temperature.map(|t| t.as_celsius()))
            .filter(|t| t.is_finite()),
        fan: fans(&data.fans),
        model: Some(format!(
            "{} {}",
            data.device_info.make, data.device_info.model
        )),
        firmware: data.firmware_version.clone(),
        power_w: data
            .wattage
            .map(|power| power.as_watts())
            .filter(|watts| watts.is_finite() && *watts > 0.0),
    }
}

/// The hottest board reading, in degrees Celsius.
fn hottest(boards: &[BoardData]) -> Option<f64> {
    boards
        .iter()
        .flat_map(|board| {
            [
                board.board_temperature,
                board.inlet_chip_temperature,
                board.outlet_chip_temperature,
            ]
        })
        .flatten()
        .map(|temperature| temperature.as_celsius())
        .filter(|celsius| celsius.is_finite())
        .reduce(f64::max)
}

/// Fan speeds as "4200/4320 rpm".
fn fans(fans: &[FanData]) -> Option<String> {
    let speeds: Vec<String> = fans
        .iter()
        .filter_map(|fan| fan.rpm)
        .map(|rpm| rpm.as_rpm())
        .filter(|rpm| rpm.is_finite())
        .map(|rpm| format!("{rpm:.0}"))
        .collect();
    (!speeds.is_empty()).then(|| format!("{} rpm", speeds.join("/")))
}

/// asic-rs's report with Pickaxe's own reading where it has one for the hash
/// rate, temperature, fan and power: the device's windowed rate (the one its
/// own app shows), the hottest reading, the fan as the device states it, and
/// Avalon Nano power.
fn mix(own: Option<DeviceReport>, found: Option<DeviceReport>) -> Option<DeviceReport> {
    let own = own.unwrap_or_default();
    let found = found.unwrap_or_default();
    let report = DeviceReport {
        hashrate: own.hashrate.or(found.hashrate),
        temperature_c: own.temperature_c.or(found.temperature_c),
        fan: own.fan.or(found.fan),
        model: found.model.or(own.model),
        firmware: found.firmware.or(own.firmware),
        power_w: own.power_w.or(found.power_w),
    };
    (!report.is_empty()).then_some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use asic_rs::core::data::{device::HashAlgorithm, hashrate::HashRate};
    use measurements::{AngularVelocity, Temperature};

    #[test]
    fn own_readings_lead_for_rate_temperature_and_fan_and_asic_rs_fills_the_rest() {
        let own = DeviceReport {
            hashrate: Some(4.176e12),
            temperature_c: Some(95.0),
            fan: Some("75%".into()),
            power_w: Some(126.0),
            ..DeviceReport::default()
        };
        let found = DeviceReport {
            hashrate: Some(4.55e12),
            temperature_c: Some(80.0),
            fan: Some("5280 rpm".into()),
            model: Some("Avalonminer AvalonNano3".into()),
            firmware: Some("25103101_0736b2e".into()),
            // asic-rs's reading of a Nano 3: its input voltage, 27.56 V.
            power_w: Some(2756.0),
        };
        let mixed = mix(Some(own.clone()), Some(found.clone())).unwrap();
        assert_eq!(mixed.hashrate, Some(4.176e12));
        assert_eq!(mixed.temperature_c, Some(95.0));
        assert_eq!(mixed.fan.as_deref(), Some("75%"));
        assert_eq!(mixed.model.as_deref(), Some("Avalonminer AvalonNano3"));
        assert_eq!(mixed.firmware.as_deref(), Some("25103101_0736b2e"));
        assert_eq!(mixed.power_w, Some(126.0));
        // A make only asic-rs reads, or only Pickaxe's own reader.
        assert_eq!(mix(None, Some(found.clone())), Some(found));
        assert_eq!(mix(Some(own.clone()), None), Some(own));
        assert_eq!(mix(None, None), None);
    }

    #[test]
    fn asic_rs_boards_and_fans_become_the_hottest_reading_and_rpm() {
        let boards = [
            BoardData {
                board_temperature: Some(Temperature::from_celsius(61.0)),
                outlet_chip_temperature: Some(Temperature::from_celsius(78.5)),
                ..BoardData::default()
            },
            BoardData {
                inlet_chip_temperature: Some(Temperature::from_celsius(70.0)),
                ..BoardData::default()
            },
        ];
        assert_eq!(hottest(&boards), Some(78.5));
        assert_eq!(hottest(&[]), None);
        let fan = |rpm: Option<f64>| FanData {
            position: 0,
            rpm: rpm.map(AngularVelocity::from_rpm),
        };
        assert_eq!(
            fans(&[fan(Some(4200.0)), fan(None), fan(Some(4320.4))]).as_deref(),
            Some("4200/4320 rpm")
        );
        assert_eq!(fans(&[fan(None)]), None);
        let rate = HashRate {
            value: 4.5,
            unit: HashRateUnit::TeraHash,
            algo: HashAlgorithm::SHA256,
        };
        assert_eq!(rate.as_unit(HashRateUnit::Hash).value, 4.5e12);
    }

    /// Opt-in and read-only: one real device on the local network, e.g.
    /// `PICKAXE_TEST_ASIC=192.168.0.127 cargo test --features stratum-v2 --lib
    /// real_device_is_identified_and_reported -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs a mining device on the local network"]
    fn real_device_is_identified_and_reported() {
        let ip: IpAddr = std::env::var("PICKAXE_TEST_ASIC")
            .expect("set PICKAXE_TEST_ASIC to the device's address")
            .parse()
            .expect("PICKAXE_TEST_ASIC must be an IP address");
        let fleet = Fleet::new();
        let reports = fleet.poll(&[(1, ip)], &AtomicBool::new(false));
        println!("report: {:?}", reports[0].1);
        println!("controls: {:?}", fleet.controls(ip));
        let report = reports[0].1.as_ref().expect("the device answered");
        assert!(report.model.is_some(), "asic-rs identified the device");
        assert!(report.hashrate.is_some());
    }

    #[test]
    fn unidentified_devices_offer_pickaxes_own_actions_and_public_ones_none() {
        let fleet = Fleet::new();
        let ip: IpAddr = "192.168.7.9".parse().unwrap();
        // #### PR #42: `controls` (no device is asked) replaces `actions`.
        assert_eq!(
            fleet.controls(ip),
            DeviceControls {
                firmware: None,
                model: None,
                actions: DeviceAction::OWN.to_vec(),
                fan: None,
                power: None,
            }
        );
        assert!(fleet
            .control("8.8.8.8".parse().unwrap(), DeviceAction::Restart)
            .is_err());
        // Public and loopback addresses are never asked.
        let stop = AtomicBool::new(false);
        let reports = fleet.poll(&[(1, "8.8.8.8".parse().unwrap())], &stop);
        assert_eq!(reports, vec![(1, None)]);
        // #### PR #42: nor identified, even freshly, and they offer nothing.
        for other in ["8.8.8.8", "127.0.0.1"] {
            let started = Instant::now();
            let ip: IpAddr = other.parse().unwrap();
            for fresh in [false, true] {
                assert_eq!(fleet.identify_now(ip, fresh), DeviceControls::default());
            }
            assert_eq!(fleet.controls(ip), DeviceControls::default());
            assert!(started.elapsed() < Duration::from_secs(1));
        }
    }

    // #### PR #42
    // What: the device the Device panel identified stays known through the
    // poller's passes, which ask only connected devices, so a confirmed
    // action on an offline row goes the way the panel listed; a device no
    // panel holds is still forgotten. Identifying and acting do not panic
    // on tokio's timer.
    // Why: each pass forgot every device that was not connected, and Pause,
    // Blink or Restart on an offline row then failed or took Pickaxe's own
    // path; a time limit made outside the runtime panicked.
    // Look here if: keep_only(), hold(), identify_now(), control() or
    // run_within_answer() changes.
    #[test]
    fn the_panels_device_stays_identified_through_polls() {
        use asic_rs::{
            avalonminer::{backends::AvalonMiner, firmware::AvalonStockFirmware},
            core::traits::{firmware::MinerFirmware, miner::MinerConstructor},
        };
        type AvalonModel = <AvalonStockFirmware as MinerFirmware>::Model;
        let fleet = Fleet::new();
        let held: IpAddr = "192.168.7.9".parse().unwrap();
        let other: IpAddr = "192.168.7.10".parse().unwrap();
        // Building asic-rs's miners sends nothing to these addresses.
        for ip in [held, other] {
            let model: AvalonModel = "1566".parse().unwrap();
            let miner = AvalonMiner::new(ip, model, None);
            fleet
                .shared
                .known
                .lock()
                .unwrap()
                .insert(ip, Known::Miner(Arc::from(miner)));
        }
        // The panel opens on the row: asic-rs knows the device, so nothing
        // is sent.
        let controls = fleet.identify_now(held, false);
        assert_eq!(controls.firmware.as_deref(), Some("AvalonMiner Stock"));
        assert!(controls.actions.contains(&DeviceAction::Pause));
        // A pass with no device connected.
        assert!(fleet.poll(&[], &AtomicBool::new(false)).is_empty());
        assert_eq!(fleet.controls(held), controls);
        assert!(fleet.through_asic_rs(held, DeviceAction::Pause).is_some());
        assert!(fleet
            .through_asic_rs(held, DeviceAction::LocateOn)
            .is_some());
        assert!(
            fleet.through_asic_rs(held, DeviceAction::Restart).is_none(),
            "an Avalon restart is Canaan's own reboot"
        );
        assert!(
            fleet.through_asic_rs(other, DeviceAction::Pause).is_none(),
            "a device no panel holds is forgotten"
        );
        assert_eq!(fleet.controls(other).firmware, None);
        // The time limit around an asic-rs action is made inside the runtime
        // (made outside, tokio panicked). An action asic-rs does not send
        // returns at once, so nothing reaches the device.
        let miner = fleet.through_asic_rs(held, DeviceAction::Pause).unwrap();
        assert_eq!(
            run_within_answer(
                fleet.runtime.as_ref().unwrap(),
                &*miner,
                DeviceAction::LowerPower
            ),
            Err("this action is not sent through asic-rs".to_owned())
        );
    }

    // #### PR #42
    // What: the fan and power settings each kind of device offers, and that
    // an Avalon's go to Pickaxe's own commands, never through asic-rs.
    // Look here if: settings_offered, supports or own_api changes.
    #[test]
    fn fan_and_power_settings_follow_the_firmware_and_avalon_help() {
        use asic_rs::{
            avalonminer::{backends::AvalonMiner, firmware::AvalonStockFirmware},
            core::traits::{firmware::MinerFirmware, miner::MinerConstructor},
        };
        let caps = |own, firmware: &str, fan_config, power_limit, tuning_config| Capabilities {
            own,
            firmware: firmware.into(),
            fan_config,
            power_limit,
            tuning_config,
        };
        let avalon_fan = Some(FanRange { min: 15, max: 100 });
        let any_fan = Some(FanRange { min: 0, max: 100 });
        // asic-rs says it sets an Avalon's power limit; it is never offered.
        let avalon = caps(
            Some(OwnApi::Avalon),
            "AvalonMiner Stock",
            false,
            true,
            false,
        );
        let listed = ["fan-spd".to_owned(), "worklevel".to_owned()];
        assert_eq!(settings_offered(&avalon, Some(&listed)), (avalon_fan, None));
        assert_eq!(
            settings_offered(&avalon, None),
            (avalon_fan, Some(PowerSetting::Modes)),
            "an Avalon that lists nothing readable has the last word"
        );
        assert_eq!(
            settings_offered(&avalon, Some(&["reboot".to_owned()])),
            (None, None)
        );
        let bitaxe = caps(Some(OwnApi::AxeOs), "Bitaxe Stock", false, false, false);
        assert_eq!(settings_offered(&bitaxe, None), (any_fan, None));
        for (firmware, fan, power, tuning, offered) in [
            (
                "AntMiner Stock",
                true,
                false,
                true,
                (any_fan, Some(PowerSetting::Modes)),
            ),
            (
                "WhatsMiner Stock",
                false,
                true,
                true,
                (None, Some(PowerSetting::Watts)),
            ),
            (
                "Braiins",
                false,
                true,
                false,
                (None, Some(PowerSetting::Watts)),
            ),
            (
                "UMC OS",
                true,
                false,
                true,
                (any_fan, Some(PowerSetting::Watts)),
            ),
            ("LuxOS", false, false, false, (None, None)),
            (
                "Proto Stock",
                true,
                true,
                true,
                (any_fan, Some(PowerSetting::Watts)),
            ),
        ] {
            assert_eq!(
                settings_offered(&caps(None, firmware, fan, power, tuning), None),
                offered,
                "{firmware}"
            );
        }
        // A real Avalon object: everything goes to Pickaxe's own commands.
        type AvalonModel = <AvalonStockFirmware as MinerFirmware>::Model;
        let model: AvalonModel = "NANO3S".parse().unwrap();
        let miner = AvalonMiner::new("192.168.7.9".parse().unwrap(), model, None);
        assert_eq!(own_api(&*miner), Some(OwnApi::Avalon));
        for action in [
            DeviceAction::FanAuto,
            DeviceAction::FanPercent(60),
            DeviceAction::PowerWatts(140),
            DeviceAction::PowerMode(PowerMode::Low),
        ] {
            assert!(!supports(&*miner, action), "{action:?}");
            assert!(is_setting(action));
        }
        assert!(!is_setting(DeviceAction::Restart));
        let controls = controls_of(Some(&*miner), Some(&listed));
        assert_eq!((controls.fan, controls.power), (avalon_fan, None));
    }

    // #### PR #42
    // What: Restart on an Avalon (A-series, Nano and Home Q) is never sent
    // through asic-rs, but is still offered, and Pickaxe's own Restart sends
    // Canaan's reboot, byte for byte.
    // Why: asic-rs's Avalon restart only restarts cgminer, and its Home Q
    // has none.
    // Look here if: supports(), controls_of() or device_api::control()
    // changes.
    #[test]
    fn avalon_restart_uses_canaans_reboot() {
        use asic_rs::{
            avalonminer::{backends::AvalonMiner, firmware::AvalonStockFirmware},
            core::traits::{firmware::MinerFirmware, miner::MinerConstructor},
        };
        type AvalonModel = <AvalonStockFirmware as MinerFirmware>::Model;
        // Building asic-rs's miner sends nothing to this address.
        let ip: IpAddr = "192.168.7.9".parse().unwrap();
        for (model, asic_rs_restarts) in [("NANO3S", true), ("1566", true), ("Q", false)] {
            let model: AvalonModel = model.parse().unwrap();
            let miner = AvalonMiner::new(ip, model, None);
            assert!(is_avalon(&*miner));
            assert_eq!(miner.supports_restart(), asic_rs_restarts);
            assert!(!supports(&*miner, DeviceAction::Restart));
            let controls = controls_of(Some(&*miner), None);
            assert_eq!(controls.actions[0], DeviceAction::Restart);
            assert!(controls.actions.contains(&DeviceAction::RaisePower));
            assert_eq!(controls.firmware.as_deref(), Some("AvalonMiner Stock"));
            // #### PR #42: make and model as a report names them.
            let info = miner.get_device_info();
            assert_eq!(
                controls.model,
                Some(format!("{} {}", info.make, info.model))
            );
        }
        let nano: AvalonModel = "NANO3S".parse().unwrap();
        assert_eq!(
            controls_of(Some(&*AvalonMiner::new(ip, nano, None)), None)
                .model
                .as_deref(),
            Some("Avalonminer AvalonNano3s")
        );
        let (avalon, requests) = device_api::tests::recording_device(vec![
            br#"{"STATUS":[{"STATUS":"I","Msg":"ASC 0 set info: reboot"}],"id":1}"#,
        ]);
        let unused_web = std::net::SocketAddr::from(([127, 0, 0, 1], 9));
        assert_eq!(
            device_api::control_at(avalon, unused_web, DeviceAction::Restart).unwrap(),
            "ASC 0 set info: reboot"
        );
        assert_eq!(
            requests.recv().unwrap(),
            r#"{"command":"ascset","parameter":"0,reboot,0"}"#
        );
    }
}
