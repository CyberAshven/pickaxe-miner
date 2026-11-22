//! Runs what the setup chose, as `pickaxe` does after its terminal setup
//! (the `mine` command in src/main.rs): a hash split, ELECTRIC, a rig of a
//! GPU pool or farm, this miner's rigs, the BCH ASIC server, or PHOTON.
use crate::tui::{GpuLaunch, SetupResult};
use std::path::Path;

/// The screen that shows a session the setup chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// GPU tokens on this computer, with any rigs it coordinates.
    Tokens,
    /// This computer's GPUs mine as a rig of a coordinator.
    Rig,
    /// The BCH ASIC server.
    Asic,
}

pub fn screen_for(result: &SetupResult) -> Screen {
    if result.split.is_some() || result.electric {
        return Screen::Tokens;
    }
    match crate::tui::gpu_launch(result.server.as_ref(), &result.options) {
        Some(GpuLaunch::Rig { .. }) => Screen::Rig,
        Some(GpuLaunch::Rigs { .. } | GpuLaunch::Alone) => Screen::Tokens,
        None => Screen::Asic,
    }
}

/// Runs `result` on this thread until it stops. `cli_node_zmq`: the command
/// line named its own ZMQ block notices, which then win over the profile's.
pub fn run(result: &SetupResult, config_path: &Path, cli_node_zmq: bool) -> Result<(), String> {
    // A stopped session updates the profile it was started from.
    crate::session_control::set_active_profile(Some(result.profile_name.clone()));
    let mined = dispatch(result, config_path, cli_node_zmq);
    // Stopping asks the session to save its settings, as leaving for
    // profiles does; the window returns to its setup either way.
    crate::session_control::take_profiles_request();
    mined
}

fn dispatch(result: &SetupResult, config_path: &Path, cli_node_zmq: bool) -> Result<(), String> {
    let cfg = &result.config;
    if let Some(shares) = &result.split {
        return crate::hash_split::run(cfg, &result.gpus, shares, config_path, false);
    }
    if result.electric {
        return crate::electric::miner::run_from_setup(cfg, &result.gpus, config_path);
    }
    let profile = Some(result.profile_name.as_str());
    match crate::tui::gpu_launch(result.server.as_ref(), &result.options) {
        Some(GpuLaunch::Rig { coordinators, name }) => crate::rigs::mine_as_rig(
            &coordinators,
            name.as_deref(),
            &cfg.payout_address,
            &result.gpus,
            cfg.intensity,
            false,
            Some(&crate::mine_watch::status_path(config_path)),
        ),
        Some(GpuLaunch::Rigs {
            port,
            public,
            rigs_only,
        }) => {
            let hub = crate::rigs::start_coordinator(
                std::net::SocketAddr::from(([0, 0, 0, 0], port)),
                &config_path.with_extension("rigs-key"),
                public
                    .as_ref()
                    .map(|(fee, address)| (Some(*fee), address.as_deref())),
                cfg.network,
                &cfg.payout_address,
            )?;
            let gpus = if rigs_only { &[][..] } else { &result.gpus[..] };
            super::session::mine_runtime_token(
                cfg,
                cfg.token,
                gpus,
                Some(hub),
                config_path,
                profile,
            )
        }
        Some(GpuLaunch::Alone) => super::session::mine_runtime_token(
            cfg,
            cfg.token,
            &result.gpus,
            None,
            config_path,
            profile,
        ),
        None => serve_asics(result, config_path, cli_node_zmq),
    }
}

/// The BCH ASIC server for the setup's ASIC choice, with the profile's ZMQ
/// block notices unless the command line named its own.
fn serve_asics(result: &SetupResult, config_path: &Path, cli_node_zmq: bool) -> Result<(), String> {
    let server = result
        .server
        .as_ref()
        .ok_or("the setup chose no mining session")?;
    let action = crate::tui::asic_serve_command(server, &result.options)
        .ok_or("the setup's choice has no ASIC server")?;
    let mut run_config = result.config.clone();
    if let (false, Some(value)) = (cli_node_zmq, &result.options.node_zmq) {
        run_config.set_node_zmq(value)?;
    }
    #[cfg(feature = "stratum-v2")]
    return crate::stratum_v2::command::run(
        action,
        &run_config,
        config_path,
        false,
        false,
        Some(&result.profile_name),
    );
    #[cfg(not(feature = "stratum-v2"))]
    {
        let _ = (action, run_config, config_path);
        Err("this build does not include Stratum V2; build with --features stratum-v2".into())
    }
}
