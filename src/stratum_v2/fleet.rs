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

use super::device_api::{self, DeviceAction, DeviceReport};
use asic_rs::{
    core::{
        data::{
            board::BoardData, collector::DataField, fan::FanData, hashrate::HashRateUnit,
            miner::MinerData,
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

    /// What this device offers on its controls page: what asic-rs supports
    /// for its make and firmware, plus Avalon work levels; Pickaxe's own
    /// actions for a device asic-rs has not identified.
    pub fn actions(&self, ip: IpAddr) -> Vec<DeviceAction> {
        let Some(miner) = self.shared.miner(ip) else {
            return DeviceAction::OWN.to_vec();
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
            .filter(|action| supports(&*miner, *action)),
        );
        if is_avalon(&*miner) {
            actions.extend([DeviceAction::LowerPower, DeviceAction::RaisePower]);
        }
        actions
    }

    /// Runs one confirmed action and describes the device's answer: through
    /// asic-rs where it supports the action, otherwise Pickaxe's own code.
    pub fn control(&self, ip: IpAddr, action: DeviceAction) -> Result<String, String> {
        if !device_api::queryable(ip) {
            return Err("only devices on the local network can be controlled".into());
        }
        let miner = self
            .shared
            .miner(ip)
            .filter(|miner| supports(&**miner, action));
        match (miner, &self.runtime) {
            (Some(miner), Some(runtime)) => runtime
                .block_on(tokio::time::timeout(ANSWER, run(&*miner, action)))
                .map_err(|_| "the device did not answer in time".to_owned())?,
            _ => device_api::control(ip, action)
                .map(|reply| format!("the device replied \"{reply}\"")),
        }
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
    /// Forgets devices that left, so a farm's churn cannot grow the cache.
    fn keep_only(&self, devices: &[(u64, IpAddr)]) {
        if let Ok(mut known) = self.known.lock() {
            known.retain(|ip, _| devices.iter().any(|(_, device)| device == ip));
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
    /// again only after `RETRY`.
    async fn identify(&self, ip: IpAddr) -> Option<Arc<dyn Miner>> {
        let known = self.known.lock().ok()?.get(&ip).cloned();
        match known {
            Some(Known::Miner(miner)) => return Some(miner),
            Some(Known::Unknown(since)) if since.elapsed() < RETRY => return None,
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
            let miner = self.identify(ip).await?;
            tokio::time::timeout(ANSWER, miner.get_data_filtered(NOT_COLLECTED.to_vec()))
                .await
                .ok()
        };
        let (own, data) = tokio::join!(own, data);
        mix(own.ok().flatten(), data.as_ref().map(from_asic_rs))
    }
}

fn supports(miner: &dyn Miner, action: DeviceAction) -> bool {
    match action {
        DeviceAction::Restart => miner.supports_restart(),
        DeviceAction::Pause => miner.supports_pause(),
        DeviceAction::Resume => miner.supports_resume(),
        DeviceAction::LocateOn | DeviceAction::LocateOff => miner.supports_set_fault_light(),
        // Work levels are Pickaxe's own Canaan commands.
        DeviceAction::LowerPower | DeviceAction::RaisePower => false,
    }
}

fn is_avalon(miner: &dyn Miner) -> bool {
    miner
        .get_device_info()
        .make
        .to_ascii_lowercase()
        .starts_with("avalon")
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
        println!("actions: {:?}", fleet.actions(ip));
        let report = reports[0].1.as_ref().expect("the device answered");
        assert!(report.model.is_some(), "asic-rs identified the device");
        assert!(report.hashrate.is_some());
    }

    #[test]
    fn unidentified_devices_offer_pickaxes_own_actions_and_public_ones_none() {
        let fleet = Fleet::new();
        let ip: IpAddr = "192.168.7.9".parse().unwrap();
        assert_eq!(fleet.actions(ip), DeviceAction::OWN.to_vec());
        assert!(fleet
            .control("8.8.8.8".parse().unwrap(), DeviceAction::Restart)
            .is_err());
        // Public and loopback addresses are never asked.
        let stop = AtomicBool::new(false);
        let reports = fleet.poll(&[(1, "8.8.8.8".parse().unwrap())], &stop);
        assert_eq!(reports, vec![(1, None)]);
    }
}
