//! #### PR #61: your own BCH P2Pool node as the ASIC server's pool. The
//! server starts it (installing or updating P2Pool as its settings say),
//! mines at it as at any SV2 pool under your share address, and shows its
//! light and your node's.
use super::launch::{self, Light, Supervisor};
use super::screen::{node_light, prepare, NodeWatch, Update};
use super::{home, Settings};
use crate::config::RuntimeConfig;
use std::path::Path;

/// A running P2Pool node of your own and what the server needs from it.
pub struct OwnNode {
    supervisor: Supervisor,
    node: NodeWatch,
    /// Where the server's devices reach it: SV2 with its key pinned.
    pub url: String,
    /// The identity it pays shares to: `<payout>,p2p=<share address>`.
    pub identity: String,
    /// The installed P2Pool version.
    pub tag: String,
    /// A newer release left waiting, or why updates are unknown.
    pub update_note: Option<String>,
}

impl OwnNode {
    /// Installs or updates P2Pool as `config_path`'s P2Pool settings say,
    /// then starts it with `cfg`'s network, node and payout.
    pub fn start(cfg: &RuntimeConfig, config_path: &Path) -> Result<Self, String> {
        let home = home(config_path);
        let settings = Settings::load(&home)?;
        let (installed, update) = prepare(&home, &settings, false)?;
        let plan = launch::plan(cfg, &home, &installed, &settings)?;
        let payout = cfg.payout_address.trim();
        let share = crate::tx::p2pool_share_address(payout)?;
        let node = cfg
            .custom_node_endpoints()
            .first()
            .map(|node| (*node).to_owned())
            .ok_or("your own P2Pool node needs your BCH node")?;
        Ok(Self {
            url: plan.device_url()?,
            identity: format!("{payout},p2p={share}"),
            tag: installed.tag.clone(),
            update_note: match update {
                Update::Available(release) => Some(format!(
                    "P2Pool {} is available: `pickaxe p2pool update`",
                    release.tag
                )),
                Update::Failed(error) => Some(format!("P2Pool update failed: {error}")),
                Update::Unknown(error) => Some(format!("P2Pool updates unknown: {error}")),
                Update::UpToDate => None,
            },
            supervisor: Supervisor::start(plan.command()?, Some(launch::API_PORT)),
            node: NodeWatch::start(node, cfg.network),
        })
    }

    /// The lights the server's dashboard shows: P2Pool's, then the node's.
    /// P2Pool stops when this is dropped.
    pub fn lights(&self) -> Vec<(&'static str, Light, String)> {
        let state = self.supervisor.state();
        let (node, text) = node_light(self.node.check().as_ref());
        vec![
            (
                "P2Pool",
                state.light(),
                match &self.update_note {
                    Some(note) => format!("{} · {} · {note}", self.tag, state.summary()),
                    None => format!("{} · {}", self.tag, state.summary()),
                },
            ),
            ("Node", node, text),
        ]
    }
}
