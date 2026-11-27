//! Your own BCH P2Pool node as the ASIC server's pool. The
//! server starts it (installing or updating P2Pool as its settings say),
//! mines at it as at any SV2 pool under your share address, and shows its
//! light and your node's.
use super::launch::{self, Light, Supervisor};
use super::screen::{node_light, prepare, NodeWatch, Update};
use super::{home, Settings};
use crate::config::RuntimeConfig;
use std::path::Path;

/// What the server's flags set in P2Pool beyond its node
/// and payout (`launch::Plan`'s fields after `plan()`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// The operator's fee, signature and P2Pool's own donation.
    pub operator: Option<launch::Operator>,
    /// The Non-custodial Pool: P2Pool's own share chain, alone.
    pub standalone: bool,
    /// `--node-p2p`: the node's P2P port, for block notices without ZMQ.
    pub node_p2p: Option<String>,
}

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
    /// P2Pool runs alone on its own share chain.
    pub standalone: bool,
}

impl OwnNode {
    /// Installs or updates P2Pool as `config_path`'s P2Pool settings say,
    /// then starts it with `cfg`'s network, node and payout, and the
    /// operator's `options`.
    pub fn start(
        cfg: &RuntimeConfig,
        config_path: &Path,
        options: &Options,
    ) -> Result<Self, String> {
        if let Some(operator) = &options.operator {
            operator.validate()?;
        }
        let node_p2p = options
            .node_p2p
            .as_deref()
            .map(launch::validate_node_p2p)
            .transpose()?;
        let home = home(config_path);
        let settings = Settings::load(&home)?;
        let (installed, update) = prepare(&home, &settings, false)?;
        let mut plan = launch::plan(cfg, &home, &installed, &settings)?;
        plan.operator = options.operator.clone();
        plan.standalone = options.standalone;
        if node_p2p.is_some() {
            plan.node_p2p = node_p2p;
        }
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
            standalone: options.standalone,
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

    /// Whether P2Pool serves work now (its light is
    /// green), for the fallback pools' grace and return.
    pub fn serving(&self) -> bool {
        self.supervisor.state().is_running()
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
