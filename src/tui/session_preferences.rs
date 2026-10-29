//! #### PR #65: return to profiles with the session's pot mode, shares and
//! separate donation values. Existing wallet keys and journals stay in place.
use super::*;

/// Saves the session being left for profiles. A failure only warns: mining
/// has already stopped cleanly, and the profile picker still opens.
pub fn remember_token_session(
    cfg: &RuntimeConfig,
    gpus: &[GpuDevice],
    config_path: &Path,
    split: Vec<String>,
    electric: bool,
) {
    if let Err(error) = save_token_session(cfg, gpus, config_path, split, electric) {
        eprintln!("warning: this session was not saved to its profile: {error}");
    }
}

/// The profile this session was started from takes the values the dashboard
/// changes (intensity, address, donations, shares, pot mode) and keeps its
/// GPUs, sources and other choices. A session started from the command line
/// updates "Last token session" and never another profile.
fn save_token_session(
    cfg: &RuntimeConfig,
    gpus: &[GpuDevice],
    config_path: &Path,
    split: Vec<String>,
    electric: bool,
) -> Result<(), String> {
    let path = crate::config::profiles_path(config_path);
    let mut profiles = MiningProfiles::load_optional(&path)?;
    let kept = ProfileChoices::load(&path);
    let donation = cfg
        .token_donations
        .get(cfg.token.as_str())
        .map(|donation| (*donation != cfg.token.donation_default()).then_some(*donation));
    let launched = crate::session_control::active_profile().and_then(|name| {
        profiles
            .profiles
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&name))
    });
    let (index, name, settings, mut choices) = if let Some(index) = launched {
        let profile = &profiles.profiles[index];
        let mut settings = profile.settings.clone();
        settings.intensity = Some(cfg.intensity);
        settings.address = Some(cfg.payout_address.clone());
        if let Some(donation) = donation {
            settings.token_donation_bps = donation;
        }
        let choices = kept.get(&profile.name).cloned().unwrap_or_default();
        (Some(index), profile.name.clone(), settings, choices)
    } else if gpus.is_empty() {
        // A rigs-only coordinator started from the command line: a GPU
        // profile made from it would mine here instead of coordinating.
        return Ok(());
    } else {
        let (backend, selection) = if let Some(first) = gpus
            .first()
            .filter(|first| gpus.iter().all(|g| g.backend == first.backend))
        {
            (
                first.backend.as_str(),
                DeviceSelection::Indices(gpus.iter().map(|g| g.index).collect()),
            )
        } else {
            // Workers have joined before this call: device discovery cannot
            // compete with live kernels. Save auto's actual selection, or
            // auto's default when a device is listed under another engine.
            let available = crate::backend::mining_gpus();
            let indexes = gpus
                .iter()
                .map(|gpu| {
                    available
                        .iter()
                        .position(|g| g.backend == gpu.backend && g.index == gpu.index)
                        .map(|i| i as u32)
                })
                .collect::<Option<Vec<_>>>();
            (
                "auto",
                indexes.map_or(DeviceSelection::Default, DeviceSelection::Indices),
            )
        };
        let mut settings = SavedConfig::from_effective(backend, &selection, cfg);
        if let Some(donation) = donation {
            settings.token_donation_bps = donation;
        }
        let name = LAST_SESSION.to_owned();
        let index = profiles
            .profiles
            .iter()
            .position(|p| p.name.eq_ignore_ascii_case(&name));
        (index, name, settings, ProfileChoices::default())
    };
    choices.electric = electric;
    choices.split = split;
    choices.electric_pot_mode = cfg.electric_pot_mode;
    choices.electric_pot_chosen = cfg.electric_pot_choice().is_some();
    choices.token_donations = cfg.token_donations.clone();
    let name = profiles.upsert(index, &name, settings)?;
    ProfileChoices::save(&path, &name, &choices)?;
    profiles.save(&path)
}

/// The profile a command-line session returns to.
const LAST_SESSION: &str = "Last token session";

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(index: u32) -> GpuDevice {
        GpuDevice {
            index,
            name: "Test GPU".into(),
            vendor: "NVIDIA".into(),
            vram_bytes: None,
            backend: crate::backend::BackendKind::Cuda,
            detail: String::new(),
            integrated: false,
            ready: true,
            pci: None,
        }
    }

    // One test owns the process-wide active profile, so runs cannot race.
    #[test]
    fn returning_to_profiles_updates_only_the_launched_profile() {
        let root = std::env::temp_dir().join(format!(
            "pickaxe-profile-return-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.json");
        let path = crate::config::profiles_path(&config);
        let mut cfg = RuntimeConfig::default();
        cfg.set_payout(crate::config::DONATION_ADDRESS.into())
            .unwrap();

        // Two split profiles on the same address and network, each on its
        // own GPU; the second also keeps a P2Pool choice.
        let mut profiles = MiningProfiles::load_optional(&path).unwrap();
        for (name, device, split) in [("80/20", 0, "PHOTON=80"), ("50/50", 1, "PHOTON=50")] {
            let settings =
                SavedConfig::from_effective("cuda", &DeviceSelection::Indices(vec![device]), &cfg);
            profiles.upsert(None, name, settings).unwrap();
            let choices = ProfileChoices {
                split: vec![split.into(), "ELECTRIC=20".into()],
                p2pool: name == "50/50",
                ..Default::default()
            };
            ProfileChoices::save(&path, name, &choices).unwrap();
        }
        profiles.save(&path).unwrap();
        let before = MiningProfiles::load_optional(&path).unwrap();

        // Leaving a session started from "50/50" saves into "50/50" only.
        crate::session_control::set_active_profile(Some("50/50".into()));
        cfg.intensity = 75;
        cfg.electric_pot_mode = crate::config::ElectricPotMode::Owner;
        cfg.token_donations.insert(
            "ELECTRIC".into(),
            crate::donation::TokenDonation::from_bps(550),
        );
        let split = vec!["PHOTON=70".to_string(), "ELECTRIC=30".to_string()];
        remember_token_session(&cfg, &[gpu(0), gpu(3)], &config, split.clone(), false);
        let after = MiningProfiles::load_optional(&path).unwrap();
        assert_eq!(after.profiles.len(), 2);
        assert_eq!(after.profiles[0], before.profiles[0]);
        let saved = &after.profiles[1];
        assert_eq!(saved.name, "50/50");
        assert_eq!(saved.settings.intensity, Some(75));
        assert_eq!(saved.settings.device, before.profiles[1].settings.device);
        let choices = ProfileChoices::load(&path);
        assert_eq!(choices["50/50"].split, split);
        assert!(choices["50/50"].p2pool);
        assert_eq!(
            choices["50/50"].electric_pot_mode,
            crate::config::ElectricPotMode::Owner
        );
        assert_eq!(choices["50/50"].token_donations, cfg.token_donations);
        assert_eq!(choices["80/20"].split[0], "PHOTON=80");

        // A command-line session updates one "Last token session"; a
        // rigs-only coordinator started there saves nothing.
        crate::session_control::set_active_profile(None);
        remember_token_session(&cfg, &[gpu(0)], &config, Vec::new(), true);
        remember_token_session(&cfg, &[gpu(0)], &config, Vec::new(), true);
        remember_token_session(&cfg, &[], &config, Vec::new(), false);
        let last = MiningProfiles::load_optional(&path).unwrap();
        assert_eq!(last.profiles.len(), 3);
        assert_eq!(last.profiles[2].name, LAST_SESSION);
        assert!(ProfileChoices::load(&path)[LAST_SESSION].electric);
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
