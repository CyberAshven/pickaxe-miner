//! #### PR #65: return to profiles with the session's pot mode, shares and
//! separate donation values. Existing wallet keys and journals stay in place.
use super::*;

pub fn remember_token_session(
    cfg: &RuntimeConfig,
    gpus: &[GpuDevice],
    config_path: &Path,
    split: Vec<String>,
    electric: bool,
) -> Result<(), String> {
    let path = crate::config::profiles_path(config_path);
    let mut profiles = MiningProfiles::load_optional(&path)?;
    let kept = ProfileChoices::load(&path);
    let index = profiles.profiles.iter().position(|p| {
        p.settings.address.as_deref() == Some(cfg.payout_address.as_str())
            && p.settings.network.as_deref() == Some(cfg.network.as_str())
            && p.settings.server.is_none()
            && kept.get(&p.name).is_some_and(|c| {
                c.electric == electric
                    && c.split.is_empty() == split.is_empty()
                    && c.electric_pot_mode == cfg.electric_pot_mode
            })
    });
    let (backend, selection) = if let Some(first) = gpus
        .first()
        .filter(|first| gpus.iter().all(|g| g.backend == first.backend))
    {
        (
            first.backend.as_str(),
            DeviceSelection::Indices(gpus.iter().map(|g| g.index).collect()),
        )
    } else if gpus.is_empty() {
        ("auto", DeviceSelection::Default)
    } else {
        // Workers have joined before this call: device discovery cannot
        // compete with live kernels. Save auto's actual selection indexes.
        let available = crate::backend::mining_gpus();
        let indexes = gpus
            .iter()
            .map(|gpu| {
                available
                    .iter()
                    .position(|g| g.backend == gpu.backend && g.index == gpu.index)
                    .map(|i| i as u32)
                    .ok_or("the selected GPU is no longer available")
            })
            .collect::<Result<Vec<_>, _>>()?;
        ("auto", DeviceSelection::Indices(indexes))
    };
    let mut settings = SavedConfig::from_effective(backend, &selection, cfg);
    if let Some(donation) = cfg.token_donations.get(cfg.token.as_str()) {
        settings.token_donation_bps =
            (*donation != cfg.token.donation_minimum()).then_some(*donation);
    }
    let name = index
        .map(|i| profiles.profiles[i].name.clone())
        .unwrap_or_else(|| {
            let mut name = "Last token session".to_owned();
            let mut suffix = 2;
            while profiles
                .profiles
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&name))
                || kept.keys().any(|n| n.eq_ignore_ascii_case(&name))
            {
                name = format!("Last token session {suffix}");
                suffix += 1;
            }
            name
        });
    let name = profiles.upsert(index, &name, settings)?;
    ProfileChoices::save(
        &path,
        &name,
        &ProfileChoices {
            electric,
            split,
            electric_pot_mode: cfg.electric_pot_mode,
            token_donations: cfg.token_donations.clone(),
            ..Default::default()
        },
    )?;
    profiles.save(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_return_preserves_owner_pot_and_separate_token_donations() {
        let root = std::env::temp_dir().join(format!(
            "pickaxe-profile-return-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.json");
        let mut cfg = RuntimeConfig::default();
        cfg.set_payout(crate::config::DONATION_ADDRESS.into())
            .unwrap();
        cfg.electric_pot_mode = crate::config::ElectricPotMode::Owner;
        cfg.intensity = 75;
        cfg.token_donations.insert(
            "PHOTON".into(),
            crate::donation::TokenDonation::from_bps(450),
        );
        cfg.token_donations.insert(
            "ELECTRIC".into(),
            crate::donation::TokenDonation::from_bps(550),
        );
        let split = vec!["PHOTON=80".into(), "ELECTRIC=20".into()];
        remember_token_session(&cfg, &[], &config, split.clone(), false).unwrap();
        let path = crate::config::profiles_path(&config);
        let first = MiningProfiles::load_optional(&path).unwrap();
        assert_eq!(first.profiles.len(), 1);
        assert_eq!(first.profiles[0].settings.intensity, Some(75));
        let choices = ProfileChoices::load(&path);
        let chosen = &choices[&first.profiles[0].name];
        assert_eq!(
            chosen.electric_pot_mode,
            crate::config::ElectricPotMode::Owner
        );
        assert_eq!(chosen.token_donations, cfg.token_donations);
        assert_eq!(chosen.split, split);
        first.profiles[0].settings.validate().unwrap();
        remember_token_session(
            &cfg,
            &[],
            &config,
            vec!["PHOTON=70".into(), "ELECTRIC=30".into()],
            false,
        )
        .unwrap();
        let updated = MiningProfiles::load_optional(&path).unwrap();
        assert_eq!(updated.profiles.len(), 1);
        assert_eq!(updated.profiles[0].name, first.profiles[0].name);
        assert!(root.starts_with(std::env::temp_dir()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
