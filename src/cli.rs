//! Single Clap startup parser for Pickaxe.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "pickaxe", version, about = "Pickaxe Miner - CashToken mining")]
pub struct Cli {
    #[arg(long, global = true, value_parser = ["mainnet", "chipnet"], conflicts_with = "chipnet")]
    pub network: Option<String>,

    /// Shorthand for --network chipnet.
    #[arg(long, global = true)]
    pub chipnet: bool,

    /// Token name, category ID, or covenant locking bytecode.
    #[arg(long, global = true)]
    pub token: Option<String>,

    #[arg(long, global = true, value_parser = ["auto", "cuda", "hip", "wgpu", "opencl"])]
    pub backend: Option<String>,

    /// GPUs to mine on: all, one number from `pickaxe devices`, or a list like 0,2.
    /// By default every discrete GPU mines (integrated GPUs only when there is
    /// no discrete GPU).
    #[arg(long, global = true, value_name = "all|N|N,M", value_parser = crate::backend::DeviceSelection::parse)]
    pub device: Option<crate::backend::DeviceSelection>,

    /// Mine on integrated GPUs too when --device is all (the default).
    #[arg(long, global = true)]
    pub include_integrated: bool,

    #[arg(
        long,
        global = true,
        value_parser = clap::value_parser!(u8).range(10..=100),
        help = "GPU intensity 10..=100 (setup restores a saved profile; otherwise 100; benchmark without this flag runs 10/25/50/75/100)"
    )]
    pub intensity: Option<u8>,

    #[arg(long, global = true)]
    pub address: Option<String>,

    #[arg(long = "node-rpc", alias = "node", global = true)]
    pub node_rpc: Option<String>,

    /// #### PR #42: the node's ZMQ block notices for the ASIC server:
    /// tcp://HOST:PORT (its zmqpubhashblock) on this computer or the home
    /// network, `auto` (bitcoin.conf's, for a node on this computer; the
    /// default) or `off`. A new block then reaches devices at once.
    #[arg(long = "node-zmq", global = true, value_name = "URL")]
    pub node_zmq: Option<String>,

    /// Fulcrum servers for PHOTON jobs, comma-separated: wss:// or ws://,
    /// or #### PR #42: a home server over plain TCP (tcp://HOST:PORT, such
    /// as tcp://umbrel.local:50001), taken only on this computer or the home
    /// network.
    #[arg(long, global = true)]
    pub fulcrum: Option<String>,

    #[arg(long, global = true, value_parser = ["node", "fulcrum"])]
    pub source: Option<String>,

    #[arg(long, global = true)]
    pub no_tui: bool,

    #[arg(long, global = true)]
    pub json: bool,

    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Coordinate GPU rigs: share this miner's job with rigs that connect to
    /// this address (for example 0.0.0.0:3340). Only this miner claims.
    #[arg(long, global = true, value_name = "ADDR:PORT")]
    pub rigs_listen: Option<std::net::SocketAddr>,

    /// Mine as a rig of the coordinator at this address; it claims every win.
    /// Repeat for backup coordinators, tried in order. #### PR #42: the
    /// coordinator's one-line address, stratum2+tcp://HOST:PORT/KEY, carries
    /// its key; a bare HOST:PORT takes the next --coordinator-key.
    #[arg(
        long,
        global = true,
        value_name = "HOST:PORT",
        conflicts_with = "rigs_listen",
        action = clap::ArgAction::Append
    )]
    pub coordinator: Vec<String>,

    /// The key each coordinator prints at start, so a rig trusts only it.
    /// Give one per --coordinator, in the same order.
    #[arg(long, global = true, value_name = "KEY", action = clap::ArgAction::Append)]
    pub coordinator_key: Vec<String>,

    /// Token donation percentage, at least the token's minimum (4 for PHOTON);
    /// Advanced settings (`a`) changes it while mining.
    #[arg(long, global = true, value_name = "PERCENT")]
    pub token_donation: Option<crate::donation::TokenDonation>,

    /// With --rigs-listen: mine with the rigs only and use no GPU on this
    /// computer, so the coordinator can run on any machine. Needs --address.
    #[arg(long, global = true, requires_all = ["rigs_listen", "address"])]
    pub rigs_only: bool,

    /// #### PR #32
    /// With --rigs-listen: run a public GPU pool, where each rig mines for
    /// the --address it gives and is claimed to it; your fee is a share of
    /// each rig's mining time.
    #[arg(long, global = true, requires = "rigs_listen")]
    pub rigs_public: bool,

    /// The public GPU pool's fee: a percentage of each rig's mining time,
    /// after the donation (default 0).
    #[arg(long, global = true, value_name = "PERCENT", requires = "rigs_public")]
    pub rigs_fee: Option<crate::donation::bch::BchDonation>,

    /// Where the public GPU pool's fee goes (default: your --address).
    #[arg(long, global = true, value_name = "ADDRESS", requires = "rigs_public")]
    pub rigs_fee_address: Option<String>,

    /// This rig's name on the coordinator's dashboard (default: the
    /// computer's name).
    #[arg(long, global = true, value_name = "NAME", requires = "coordinator")]
    pub rig_name: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

// #### PR #40: parsed once at start, so the server options' size is no cost.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum Commands {
    Mine,
    /// #### PR #40
    /// Show the status of a miner running without a screen, such as a GPU
    /// farm's or pool's coordinator as a service (read-only; same --config).
    Watch,
    Devices,
    SelfTest,
    Benchmark {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long)]
        ui_compare: bool,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// BCH Stratum V2 (ASIC-facing; see docs/stratum-v2.md).
    StratumV2 {
        #[command(subcommand)]
        command: StratumV2Command,
    },
    /// #### PR #42
    /// For farm operating systems (HiveOS, mmpOS, RaveOS): their scripts run
    /// `farm-os mine` with the flight sheet's fields and read
    /// `farm-os stats` (see docs/farm.md).
    FarmOs {
        #[command(subcommand)]
        command: FarmOsCommand,
    },
    /// #### PR #51, #58
    /// ELECTRIC on your GPUs, beside BCH or PHOTON, with the relay fee paid
    /// from a pot you fund, with a dedicated recovery key (see
    /// docs/electric-token.md).
    Electric {
        #[command(subcommand)]
        command: ElectricCommand,
    },
    /// #### PR #53
    /// Mine several GPU tokens on the selected GPUs, each for its share of
    /// the time in 5% steps, for example `--share photon=80 --share
    /// electric=20` (see docs/hash-split.md).
    Split {
        /// TOKEN=PERCENT, once per token; multiples of 5, at most 100 in all.
        #[arg(long = "share", value_name = "TOKEN=PERCENT", required = true)]
        shares: Vec<String>,
        /// Keep mining the previous funded ELECTRIC pot until its owner
        /// migrates the BCH to a signed-claim pot.
        #[arg(long)]
        restricted_electric_pot: bool,
        /// New owner-controlled pot with Max/send; old funded pots remain separate.
        #[arg(long, conflicts_with = "restricted_electric_pot")]
        owner_electric_pot: bool,
        /// #### PR #65: the signed-claim ELECTRIC pot even while a previous
        /// funded pot with the same key could still be mined.
        #[arg(long, conflicts_with_all = ["restricted_electric_pot", "owner_electric_pot"])]
        signed_electric_pot: bool,
    },
    /// #### PR #53
    /// BCH P2Pool as a dependency Pickaxe installs from P2Pool's releases,
    /// keeps updated and runs with your node and payout (see docs/p2pool.md).
    P2pool {
        #[command(subcommand)]
        command: P2poolCommand,
    },
    #[command(hide = true)]
    Repl,
}

/// #### PR #53
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum P2poolCommand {
    /// Run P2Pool with --node-rpc, --node-zmq and --address on --network,
    /// restarting it when it exits, with a screen of its state and the node's.
    Run {
        /// Start the installed version without installing a newer one.
        #[arg(long)]
        no_update: bool,
    },
    /// Install the newest P2Pool release after checking it against the
    /// SHA-256 published beside it.
    Update {
        /// Only say whether a newer release is out.
        #[arg(long)]
        check: bool,
    },
    /// Go back to the P2Pool version installed before this one.
    Rollback,
    /// The installed and newest versions, and the settings.
    Status,
    /// Change the settings: automatic updates, the share chain, peers.
    Set {
        /// on (the default) or off: off, a newer release waits for
        /// `pickaxe p2pool update` or `u` on the screen.
        #[arg(long, value_name = "on|off")]
        auto_update: Option<String>,
        /// main or mini (the default; shares 256 times easier).
        #[arg(long, value_name = "main|mini")]
        share_chain: Option<String>,
        /// Another P2Pool node to join, such as /ip4/203.0.113.7/tcp/6884;
        /// repeat for more.
        #[arg(long = "peer", value_name = "ADDRESS")]
        peers: Vec<String>,
        /// Forget the saved peers (this node then starts its own share chain).
        #[arg(long)]
        no_peers: bool,
    },
}

/// #### PR #51
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum ElectricCommand {
    /// Create/reopen a separate owner-only payout wallet beside the config.
    /// Prints its public address, QR and backup path, never its private key.
    Wallet,
    /// Create/load a dedicated recovery key and print the fee pot's address.
    Pot {
        #[command(flatten)]
        deployment: ElectricDeploymentArgs,
    },
    /// Show the baton, the tip and what the pot holds.
    Status {
        #[command(flatten)]
        deployment: ElectricDeploymentArgs,
    },
    /// Search on the selected GPUs and claim from the pot.
    Mine {
        #[command(flatten)]
        deployment: ElectricDeploymentArgs,
        /// Exit after one accepted reward (or one dry-run proof).
        #[arg(long)]
        once: bool,
        /// Search no baton above this many zero bytes (3: only the cheap claims).
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..=32))]
        max_difficulty: u8,
        /// Search without a funded pot and send nothing: shows this
        /// computer's rate and how often it finds a proof.
        #[arg(long)]
        dry_run: bool,
    },
    /// Send each pot coin back to --address. Prints the transactions; add
    /// --broadcast to send them.
    Refund {
        #[command(flatten)]
        deployment: ElectricDeploymentArgs,
        #[arg(long)]
        broadcast: bool,
    },
    /// Build the genesis of your own deployment (Chipnet has none published):
    /// the baton with the whole supply at the contract. Prints the unsigned
    /// transaction for the funding coin's wallet to sign and send.
    Deploy {
        /// The funding coin, TXID:0; its txid becomes the token category.
        #[arg(long, value_name = "TXID:0")]
        funding: String,
        /// The funding coin's value in sats.
        #[arg(long, value_name = "SATS")]
        funding_sats: u64,
        /// Where the change goes (default: --address).
        #[arg(long, value_name = "CASHADDR")]
        change: Option<String>,
        /// Token units on the baton (default: one million rewards).
        #[arg(long, value_name = "UNITS")]
        supply: Option<u64>,
        /// The baton's starting height (default: the chain tip).
        #[arg(long, value_name = "HEIGHT")]
        height: Option<u32>,
        /// The contract's reward per claim (default 60000000).
        #[arg(long, value_name = "UNITS")]
        electric_reward: Option<u64>,
        /// The contract's minimum difficulty in bytes (default 3).
        #[arg(long, value_name = "BYTES")]
        electric_min_difficulty: Option<u8>,
        /// The contract's maximum difficulty in bytes (default 4).
        #[arg(long, value_name = "BYTES")]
        electric_max_difficulty: Option<u8>,
    },
}

/// #### PR #51: another deployment of the same contract, such as your own
/// Chipnet deploy (required there) or a redeploy; mainnet defaults to the
/// published one.
#[derive(clap::Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct ElectricDeploymentArgs {
    /// Pot-only recovery key file (default: beside the config, per network/pot).
    #[arg(long, value_name = "PATH", conflicts_with = "legacy_pot")]
    pub pot_key: Option<std::path::PathBuf>,
    /// Use an already-funded keyless pot from the original ELECTRIC build.
    #[arg(long)]
    pub legacy_pot: bool,
    /// Use the previous recovery-key pot, whose keyless claims wait between heights.
    /// Its funding address and refund key remain available for existing funds.
    #[arg(long, conflicts_with = "legacy_pot")]
    pub restricted_pot: bool,
    /// Create/use a separate pot whose saved key controls withdrawals to any
    /// valid BCH address. This does not migrate funds from previous pots.
    #[arg(long, conflicts_with_all = ["legacy_pot", "restricted_pot"])]
    pub owner_pot: bool,
    /// #### PR #65: the signed-claim pot even while the previous funded pot
    /// with the same key could still be mined (the default mines that one
    /// until it is migrated).
    #[arg(long, conflicts_with_all = ["legacy_pot", "restricted_pot", "owner_pot"])]
    pub signed_pot: bool,
    /// The deployment's token category ID.
    #[arg(long, value_name = "HEX")]
    pub electric_category: Option<String>,
    /// Token units per claim (default 60000000).
    #[arg(long, value_name = "UNITS")]
    pub electric_reward: Option<u64>,
    /// The contract's minimum difficulty in bytes (default 3).
    #[arg(long, value_name = "BYTES")]
    pub electric_min_difficulty: Option<u8>,
    /// The contract's maximum difficulty in bytes (default 4).
    #[arg(long, value_name = "BYTES")]
    pub electric_max_difficulty: Option<u8>,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCommand {
    Show,
    Validate,
    Save,
}

#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum StratumV2Command {
    /// Print implementation and validation status.
    Status,
    /// Check the configured BCH node and full block template without mining.
    CheckNode,
    /// Show a running server's workers table, read-only (for example a
    /// service started with --no-tui). Use the same --config as the server.
    Watch,
    /// #### PR #61: Print the BCH P2Pool share address of a P2PKH address
    /// (default: the configured payout address). A P2Pool node with no share
    /// address of its own needs one in --upstream-user, after ",p2p=".
    ShareAddress {
        /// A P2PKH CashAddr from the wallet whose key should own the shares.
        address: Option<String>,
    },
    /// Serve encrypted BCH mining jobs to SV2 devices.
    Serve {
        /// Listener address. Use a LAN address to connect an external ASIC.
        #[arg(long, default_value = "127.0.0.1:3336")]
        listen: std::net::SocketAddr,
        /// Optional plain SV1 endpoint for ASIC firmware on a trusted LAN.
        #[arg(long)]
        sv1_listen: Option<std::net::SocketAddr>,
        /// BCH donation percentage, 0 to 100 (default 1.5). Defaults to the saved setting.
        #[arg(long)]
        donation: Option<crate::donation::bch::BchDonation>,
        /// #### PR #40
        /// Mine at a remote SV2 pool instead of your own node: HOST:PORT of
        /// the pool (another Pickaxe server, or a BCH SV2 pool). SV1 devices
        /// connect to --sv1-listen; no node is needed. Repeat for backup
        /// pools, tried in order.
        /// #### PR #40: HOST:PORT, or the pool's one-line SV2 address with
        /// its key, stratum2+tcp://HOST:PORT/KEY.
        #[arg(long, requires = "sv1_listen")]
        upstream: Vec<String>,
        /// The pool's authority public key, as the pool publishes it; one per
        /// --upstream, in the same order. Not needed when each --upstream
        /// carries its key.
        #[arg(long, requires = "upstream")]
        upstream_key: Vec<String>,
        /// The identity the pool knows you by: an account or worker name, or
        /// for a solo pool your payout address. Defaults to the configured
        /// payout address.
        #[arg(long, requires = "upstream")]
        upstream_user: Option<String>,
        /// #### PR #61
        /// Mine at your own BCH P2Pool node, which Pickaxe installs, keeps
        /// updated and runs with --node-rpc, --node-zmq and --address (see
        /// docs/p2pool.md). SV1 devices connect to --sv1-listen.
        #[arg(long, requires = "sv1_listen", conflicts_with_all = ["upstream", "public"])]
        own_p2pool: bool,
        /// #### PR #40
        /// Run a public pool: each miner's username is their own payout
        /// address (q or p, optionally with .worker) and the blocks they find
        /// pay them; the Pickaxe donation comes off first, then your fee.
        #[arg(long, conflicts_with = "upstream")]
        public: bool,
        /// The public pool's fee: a percentage of what the donation leaves,
        /// 0 to 100 (default 0).
        #[arg(long, requires = "public")]
        pool_fee: Option<crate::donation::bch::BchDonation>,
        /// Where the fee comes from: coinbase, work or both (default coinbase).
        #[arg(long, requires = "public")]
        pool_fee_mode: Option<crate::donation::bch::FeeMode>,
        /// The fee's address, q or p (such as a multisig); defaults to the
        /// configured payout address.
        #[arg(long, requires = "public")]
        pool_fee_address: Option<String>,
        /// #### PR #40
        /// The pool's name, written into the coinbase of every block this
        /// server builds (at most 20 printable characters), such as /MyPool/.
        /// Not with --upstream: the pool there builds the blocks.
        #[arg(long, value_name = "TEXT", conflicts_with = "upstream")]
        pool_tag: Option<String>,
        /// #### PR #40
        /// The share difficulty every device starts at (default 4096), for
        /// farms of fast ASICs; vardiff moves each device from there.
        #[arg(long, value_name = "DIFFICULTY", conflicts_with = "upstream")]
        start_difficulty: Option<u64>,
        /// #### PR #42
        /// Merge-mines the Chipnet test token at this share difficulty, to
        /// try merge mining with real devices before any token exists;
        /// refused on mainnet. With --upstream it needs --job-declaration
        /// (the pool builds the blocks otherwise). Hidden: it is for tests.
        #[arg(long, value_name = "DIFFICULTY", hide = true)]
        merge_test_token: Option<u64>,
        /// #### merge-mining v2
        /// Merge-mines tSAFA, the Chipnet test deployment of BCH P2Pool's
        /// SAFA AuxPoW covenant, from this deployment file (JSON), and
        /// claims its wins through Fulcrum. Chipnet only, with this
        /// server's own templates (solo or --public). Hidden: it is for
        /// tests.
        #[arg(
            long,
            value_name = "FILE",
            hide = true,
            conflicts_with_all = ["upstream", "merge_test_token", "asic_token", "asic_test_token"]
        )]
        safa_deployment: Option<std::path::PathBuf>,
        /// #### merge-mining v2
        /// How long after a merge-mined token win its claim may be
        /// broadcast: SECONDS, or MIN-MAX seconds drawn at random for each
        /// claim (default 5-60). Each claim is its own transaction. To hide
        /// where claims come from, route this computer through Tor yourself.
        #[arg(long, value_name = "SECONDS")]
        claim_delay: Option<String>,
        /// #### PR #42
        /// Serve this node's block templates to SV2 pools, Job Declaration
        /// clients and P2Pool (SV2 Template Distribution) at this address,
        /// such as 0.0.0.0:8442 (mainnet) or 0.0.0.0:48442 (Chipnet), with
        /// the mining listener's key. Off by default. Not with --upstream: a
        /// pool's templates come from its own node.
        #[arg(long, value_name = "ADDRESS", conflicts_with = "upstream")]
        tp_listen: Option<std::net::SocketAddr>,
        /// #### PR #42
        /// Accept miners' own templates at this public pool (SV2 Job
        /// Declaration) on the SV2 port: `full` checks each declared
        /// template with your node and sends its blocks to your node too,
        /// `coinbase` lets a miner set its own jobs without showing their
        /// transactions, `both` (the flag alone) takes either. A miner's
        /// coinbase must pay your fee and the Pickaxe donation in full. Off
        /// by default.
        #[arg(
            long,
            value_name = "MODE",
            num_args = 0..=1,
            default_missing_value = "both",
            requires = "public"
        )]
        accept_job_declaration: Option<AcceptJobDeclaration>,
        /// #### PR #42
        /// Mine at the first --upstream pool with your own node's templates
        /// (SV2 Job Declaration), at a Pickaxe pool that accepts it: `full`
        /// (the flag alone) declares each template with its transactions,
        /// so the pool checks it and sends its blocks to its node too;
        /// `coinbase` declares the coinbase only, and your node alone
        /// submits the blocks. While the pool refuses them, devices mine the
        /// pool's own jobs. Needs your node, like solo mining.
        #[arg(
            long,
            value_name = "MODE",
            num_args = 0..=1,
            default_missing_value = "full",
            requires = "upstream"
        )]
        job_declaration: Option<JobDeclarationMode>,
        /// Verified P2Pool node's read-only /jd_prepare URL. Payout and share
        /// commitments are prepared there, using the allocated pool token.
        #[arg(long, value_name = "URL", requires = "job_declaration")]
        p2pool_prepare_api: Option<String>,
        /// #### PR #42
        /// Take templates from an SV2 Template Provider, tried before your
        /// node: sv2tp://HOST:PORT/KEY, such as another Pickaxe's
        /// --tp-listen. Repeat for more, tried in order. The key may be left
        /// out only for a provider on this computer.
        #[arg(long, value_name = "URL")]
        template_provider: Vec<String>,
        /// #### PR #42
        /// Solo mining with a pool to fall back on: SV1 devices mine at this
        /// pool (stratum2+tcp://HOST:PORT/KEY) while your node gives no work,
        /// and come back once it has served for 30 seconds. Repeat for more,
        /// tried in order; the donation works there as at any pool. Needs
        /// --sv1-listen; not with --upstream or --public.
        #[arg(
            long,
            value_name = "ADDRESS",
            requires = "sv1_listen",
            conflicts_with_all = ["upstream", "public"]
        )]
        fallback_pool: Vec<String>,
        /// #### PR #42
        /// Mine an ASIC-exclusive token instead of BCH: devices mine its
        /// header-shaped jobs, and no node is needed. Refused until a token
        /// is registered on the network (none is yet).
        #[arg(
            long,
            value_name = "NAME",
            conflicts_with_all = [
                "upstream",
                "public",
                "tp_listen",
                "job_declaration",
                "accept_job_declaration",
                "merge_test_token",
                "template_provider",
                "fallback_pool",
                "asic_test_token"
            ]
        )]
        asic_token: Option<String>,
        /// #### PR #42
        /// Mines the Chipnet ASIC test token (SAFA layout, a simulated
        /// thread) at this share difficulty instead of BCH, to try
        /// ASIC-exclusive mining before any such token exists; refused on
        /// mainnet. Hidden: it is for tests.
        #[arg(
            long,
            value_name = "DIFFICULTY",
            hide = true,
            conflicts_with_all = [
                "upstream",
                "public",
                "tp_listen",
                "job_declaration",
                "accept_job_declaration",
                "merge_test_token",
                "template_provider",
                "fallback_pool"
            ]
        )]
        asic_test_token: Option<u64>,
    },
}

/// #### PR #42: how a miner declares its templates to a pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum JobDeclarationMode {
    /// Full-Template: each template with its transactions; the pool checks
    /// it and sends its blocks to its node too.
    Full,
    /// Coinbase-only: custom jobs from tokens; the pool never sees the
    /// transactions, and this node submits the blocks.
    Coinbase,
    /// Full-Template with no fallback to the pool's transaction selection.
    FullRequired,
    /// Coinbase-only with no fallback to the pool's transaction selection.
    CoinbaseRequired,
}

impl JobDeclarationMode {
    pub fn requires_own_templates(self) -> bool {
        matches!(self, Self::FullRequired | Self::CoinbaseRequired)
    }
}

/// #### PR #42: the Job Declaration modes a public pool accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum AcceptJobDeclaration {
    /// Full-Template: a miner declares each template with its
    /// transactions; the pool's node checks it and gets its blocks too.
    Full,
    /// Coinbase-only: a miner sets jobs from tokens the pool allocates; the
    /// pool never sees their transactions, and the miner's node submits
    /// their blocks.
    Coinbase,
    /// Either, as each miner asks.
    Both,
}

/// #### PR #42: what a farm operating system's scripts run.
#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum FarmOsCommand {
    /// Print the running miner's statistics in the system's format (from
    /// the status file beside --config).
    Stats {
        #[arg(long)]
        os: crate::farm_os::FarmOs,
    },
    /// Mine with the flight sheet's --pool, --user and --password (and
    /// --coin, --api-port, --pool-protocol, which are ignored); other flags
    /// pass through to `mine`.
    Mine {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// Parses command-line arguments into the supported miner commands;
/// #### PR #42: `farm-os mine ...` becomes Pickaxe's own `mine` first (see
/// `farm_os::mine_argv`).
pub fn parse() -> Cli {
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if raw.get(1).is_some_and(|arg| arg == "farm-os") && raw.get(2).is_some_and(|arg| arg == "mine")
    {
        let rest: Vec<String> = raw[3..]
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        match crate::farm_os::mine_argv(&rest) {
            Ok(argv) => return Cli::parse_from(argv),
            Err(error) => {
                eprintln!("error: {error}");
                std::process::exit(2);
            }
        }
    }
    Cli::parse()
}

#[cfg(test)]
mod tests {
    #[test]
    fn electric_key_options_are_shared_and_exclude_legacy_mode() {
        use clap::Parser;
        for command in ["pot", "status", "mine", "refund"] {
            assert!(super::Cli::try_parse_from([
                "pickaxe",
                "electric",
                command,
                "--pot-key",
                "backup.pot-key"
            ])
            .is_ok());
            assert!(
                super::Cli::try_parse_from(["pickaxe", "electric", command, "--legacy-pot"])
                    .is_ok()
            );
            assert!(super::Cli::try_parse_from([
                "pickaxe",
                "electric",
                command,
                "--legacy-pot",
                "--pot-key",
                "backup.pot-key"
            ])
            .is_err());
        }
    }
    use super::*;

    #[test]
    fn required_command_shape_parses() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--backend",
            "cuda",
            "--device",
            "0",
            "--intensity",
            "75",
            "--no-tui",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(Commands::Mine)));
        assert_eq!(cli.backend.as_deref(), Some("cuda"));
        assert_eq!(
            cli.device,
            Some(crate::backend::DeviceSelection::Indices(vec![0]))
        );
        assert_eq!(cli.intensity, Some(75));
        assert!(cli.no_tui);

        let show = Cli::try_parse_from(["pickaxe", "config", "show"]).unwrap();
        assert!(matches!(
            show.command,
            Some(Commands::Config {
                command: ConfigCommand::Show
            })
        ));

        let save =
            Cli::try_parse_from(["pickaxe", "config", "save", "--config", "pickaxe.json"]).unwrap();
        assert!(matches!(
            save.command,
            Some(Commands::Config {
                command: ConfigCommand::Save
            })
        ));
        assert_eq!(save.config, Some(PathBuf::from("pickaxe.json")));
    }

    // #### PR #42
    // What: the argv `farm-os mine` rewrites to parses as `mine`, headless,
    // with the rig's coordinator and name; `farm-os stats --os` parses.
    // Look here if: farm_os::mine_argv or the farm-os subcommand changes.
    #[test]
    fn the_rewritten_farm_command_parses_as_mine() {
        let argv = crate::farm_os::mine_argv(&[
            "--pool".into(),
            "stratum2+tcp://192.0.2.1:3340/KEY".into(),
            "--user".into(),
            "bitcoincash:qp.rack-1".into(),
            "--intensity".into(),
            "90".into(),
        ])
        .unwrap();
        let cli = Cli::try_parse_from(argv).unwrap();
        assert!(matches!(cli.command, Some(Commands::Mine)));
        assert!(cli.no_tui);
        assert_eq!(cli.coordinator, ["stratum2+tcp://192.0.2.1:3340/KEY"]);
        assert_eq!(cli.rig_name.as_deref(), Some("rack-1"));
        assert_eq!(cli.intensity, Some(90));
        let stats = Cli::try_parse_from(["pickaxe", "farm-os", "stats", "--os", "hiveos"]).unwrap();
        assert!(matches!(
            stats.command,
            Some(Commands::FarmOs {
                command: FarmOsCommand::Stats {
                    os: crate::farm_os::FarmOs::Hiveos
                }
            })
        ));
    }

    #[test]
    fn clap_rejects_out_of_range_intensity() {
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--intensity", "9"]).is_err());
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--intensity", "101"]).is_err());
    }

    #[test]
    fn device_flag_takes_all_a_number_or_a_list() {
        use crate::backend::DeviceSelection;
        let parse = |args: &[&str]| Cli::try_parse_from(args).map(|cli| cli.device);
        assert_eq!(
            parse(&["pickaxe", "mine", "--device", "all"]).unwrap(),
            Some(DeviceSelection::Default)
        );
        assert_eq!(
            parse(&["pickaxe", "mine", "--device", "0,2"]).unwrap(),
            Some(DeviceSelection::Indices(vec![0, 2]))
        );
        assert!(parse(&["pickaxe", "mine", "--device", "gpu0"]).is_err());
        assert!(parse(&["pickaxe", "mine", "--device", "1,1"]).is_err());
        let cli = Cli::try_parse_from(["pickaxe", "mine", "--include-integrated"]).unwrap();
        assert!(cli.include_integrated);
        assert_eq!(cli.device, None);
    }

    #[test]
    fn clap_accepts_wgpu_backend_surface() {
        let cli = Cli::try_parse_from(["pickaxe", "devices", "--backend", "wgpu"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::Devices)));
        assert_eq!(cli.backend.as_deref(), Some("wgpu"));
        let cli = Cli::try_parse_from(["pickaxe", "devices", "--backend", "opencl"]).unwrap();
        assert_eq!(cli.backend.as_deref(), Some("opencl"));
    }

    #[test]
    fn clap_parses_offline_self_test() {
        let cli = Cli::try_parse_from(["pickaxe", "self-test", "--backend", "cuda"]).unwrap();
        assert!(matches!(cli.command, Some(Commands::SelfTest)));
        assert_eq!(cli.backend.as_deref(), Some("cuda"));
    }

    #[test]
    fn clap_parses_offline_benchmark_window() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "benchmark",
            "--backend",
            "cuda",
            "--seconds",
            "7",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Benchmark {
                seconds: 7,
                ui_compare: false
            })
        ));
        assert_eq!(cli.backend.as_deref(), Some("cuda"));
        assert_eq!(cli.intensity, None);

        let selected = Cli::try_parse_from([
            "pickaxe",
            "benchmark",
            "--backend",
            "cuda",
            "--seconds",
            "3",
            "--intensity",
            "30",
        ])
        .unwrap();
        assert_eq!(selected.intensity, Some(30));

        let ui = Cli::try_parse_from([
            "pickaxe",
            "benchmark",
            "--backend",
            "cuda",
            "--seconds",
            "3",
            "--ui-compare",
        ])
        .unwrap();
        assert!(matches!(
            ui.command,
            Some(Commands::Benchmark {
                seconds: 3,
                ui_compare: true
            })
        ));
    }

    #[test]
    fn clap_rejects_removed_live_dry_run_flag() {
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--dry-run"]).is_err());
    }

    #[test]
    fn mining_network_and_token_flags_parse_after_command() {
        let cli =
            Cli::try_parse_from(["pickaxe", "mine", "--chipnet", "--token", "PHOTON"]).unwrap();
        assert!(cli.chipnet);
        assert_eq!(cli.token.as_deref(), Some("PHOTON"));
        assert!(
            Cli::try_parse_from(["pickaxe", "mine", "--chipnet", "--network", "mainnet"]).is_err()
        );
    }

    #[test]
    fn rig_flags_parse_and_a_rig_needs_the_coordinator_key() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--coordinator",
            "192.0.2.1:3340",
            "--coordinator-key",
            "key",
        ])
        .unwrap();
        assert_eq!(cli.coordinator, ["192.0.2.1:3340"]);
        let cli = Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--coordinator",
            "192.0.2.1:3340",
            "--coordinator-key",
            "main",
            "--coordinator",
            "192.0.2.2:3340",
            "--coordinator-key",
            "backup",
        ])
        .unwrap();
        assert_eq!(cli.coordinator, ["192.0.2.1:3340", "192.0.2.2:3340"]);
        assert_eq!(cli.coordinator_key, ["main", "backup"]);
        // #### PR #32
        // A rig may be named; a coordinator may mine with its rigs only, given
        // where it pays.
        let named = Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--coordinator",
            "192.0.2.1:3340",
            "--coordinator-key",
            "main",
            "--rig-name",
            "rack-1",
        ])
        .unwrap();
        assert_eq!(named.rig_name.as_deref(), Some("rack-1"));
        assert!(Cli::try_parse_from(["pickaxe", "mine", "--rig-name", "rack-1"]).is_err());
        let only = Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--rigs-listen",
            "0.0.0.0:3340",
            "--rigs-only",
            "--address",
            "payout",
        ])
        .unwrap();
        assert!(only.rigs_only);
        assert!(Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--rigs-listen",
            "0.0.0.0:3340",
            "--rigs-only"
        ])
        .is_err());
        assert!(
            Cli::try_parse_from(["pickaxe", "mine", "--rigs-only", "--address", "payout"]).is_err()
        );
        // #### PR #42: a bare address parses; the pairing with its key is
        // checked when the rig starts (`coordinators` in main.rs).
        assert!(
            Cli::try_parse_from(["pickaxe", "mine", "--coordinator", "192.0.2.1:3340"]).is_ok()
        );
        let cli =
            Cli::try_parse_from(["pickaxe", "mine", "--rigs-listen", "0.0.0.0:3340"]).unwrap();
        assert_eq!(cli.rigs_listen, Some("0.0.0.0:3340".parse().unwrap()));
        assert!(Cli::try_parse_from([
            "pickaxe",
            "mine",
            "--rigs-listen",
            "0.0.0.0:3340",
            "--coordinator",
            "192.0.2.1:3340",
            "--coordinator-key",
            "key",
        ])
        .is_err());
    }

    #[test]
    fn clap_parses_stratum_v2_status() {
        let cli = Cli::try_parse_from(["pickaxe", "stratum-v2", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Status
            })
        ));
    }

    #[test]
    fn clap_parses_stratum_v2_watch_without_server_options() {
        let cli = Cli::try_parse_from(["pickaxe", "--chipnet", "stratum-v2", "watch"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Watch
            })
        ));
    }

    #[test]
    fn bch_donation_flag_accepts_percentages_from_zero_to_one_hundred() {
        for (text, expected) in [("0", 0), ("1.5", 150), ("2.01", 201), ("100", 10_000)] {
            let cli = Cli::try_parse_from(["pickaxe", "stratum-v2", "serve", "--donation", text])
                .unwrap();
            let Some(Commands::StratumV2 {
                command:
                    StratumV2Command::Serve {
                        donation: Some(rate),
                        ..
                    },
            }) = cli.command
            else {
                panic!("donation missing")
            };
            assert_eq!(u16::from(rate), expected);
        }
        for text in ["-1", "100.01", "2.001", "NaN"] {
            assert!(
                Cli::try_parse_from(["pickaxe", "stratum-v2", "serve", "--donation", text])
                    .is_err()
            );
        }
    }

    // #### PR #42
    // What: --template-provider repeats, in order.
    // Look here if: the Serve flags change.
    #[test]
    fn template_providers_parse_in_order() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--template-provider",
            "sv2tp://127.0.0.1:48442",
            "--template-provider",
            "sv2tp://tp.example:8442/KEY",
        ])
        .unwrap();
        let Some(Commands::StratumV2 {
            command: StratumV2Command::Serve {
                template_provider, ..
            },
        }) = cli.command
        else {
            panic!("serve options missing")
        };
        assert_eq!(
            template_provider,
            ["sv2tp://127.0.0.1:48442", "sv2tp://tp.example:8442/KEY"]
        );
    }

    // #### PR #42
    // What: --tp-listen takes an address and cannot be used with --upstream,
    // where the pool's templates come from its own node.
    // Look here if: the Serve flags change.
    #[test]
    fn tp_listen_serves_templates_and_conflicts_with_upstream() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--tp-listen",
            "0.0.0.0:48442",
        ])
        .unwrap();
        let Some(Commands::StratumV2 {
            command: StratumV2Command::Serve { tp_listen, .. },
        }) = cli.command
        else {
            panic!("serve options missing")
        };
        assert_eq!(tp_listen, Some("0.0.0.0:48442".parse().unwrap()));
        assert!(Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--tp-listen",
            "0.0.0.0:48442",
            "--upstream",
            "stratum2+tcp://pool.example:3336/KEY",
            "--sv1-listen",
            "0.0.0.0:3333",
        ])
        .is_err());
    }

    // #### PR #42
    // What: --accept-job-declaration needs --public and alone means both;
    // --job-declaration needs --upstream and alone means full.
    // Look here if: the Serve flags change.
    #[test]
    fn accepting_job_declaration_needs_a_public_pool() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--public",
            "--accept-job-declaration",
            "coinbase",
        ])
        .unwrap();
        let Some(Commands::StratumV2 {
            command:
                StratumV2Command::Serve {
                    accept_job_declaration,
                    ..
                },
        }) = cli.command
        else {
            panic!("serve options missing")
        };
        assert_eq!(accept_job_declaration, Some(AcceptJobDeclaration::Coinbase));
        assert!(
            Cli::try_parse_from([
                "pickaxe",
                "stratum-v2",
                "serve",
                "--job-declaration",
                "coinbase",
            ])
            .is_err(),
            "declaring templates needs a pool"
        );
        assert!(Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--upstream",
            "stratum2+tcp://pool.example:3336/KEY",
            "--sv1-listen",
            "0.0.0.0:3333",
            "--job-declaration",
            "coinbase",
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--accept-job-declaration",
            "coinbase",
        ])
        .is_err());
        let modes = |args: &[&str]| {
            let cli = Cli::try_parse_from(
                ["pickaxe", "stratum-v2", "serve"]
                    .iter()
                    .chain(args)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            let Some(Commands::StratumV2 {
                command:
                    StratumV2Command::Serve {
                        accept_job_declaration,
                        job_declaration,
                        ..
                    },
            }) = cli.command
            else {
                panic!("serve options missing")
            };
            (accept_job_declaration, job_declaration)
        };
        assert_eq!(
            modes(&["--public", "--accept-job-declaration"]),
            (Some(AcceptJobDeclaration::Both), None)
        );
        assert_eq!(
            modes(&["--public", "--accept-job-declaration", "full"]),
            (Some(AcceptJobDeclaration::Full), None)
        );
        assert_eq!(
            modes(&[
                "--upstream",
                "stratum2+tcp://pool.example:3336/KEY",
                "--sv1-listen",
                "0.0.0.0:3333",
                "--job-declaration",
            ]),
            (None, Some(JobDeclarationMode::Full))
        );
    }

    // #### PR #42
    // What: --asic-test-token and --asic-token mine a token instead of BCH,
    // so they conflict with joining or running a pool, serving templates,
    // Job Declaration, the merge-mining test token, template providers and
    // fallback pools.
    // Look here if: the Serve flags change.
    #[test]
    fn asic_tokens_exclude_bch_only_modes() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(
                ["pickaxe", "stratum-v2", "serve"]
                    .iter()
                    .chain(args)
                    .collect::<Vec<_>>(),
            )
        };
        assert!(parse(&["--asic-test-token", "1000"]).is_ok());
        assert!(parse(&["--asic-token", "SAFA"]).is_ok());
        for other in [
            &["--public"][..],
            &["--merge-test-token", "1000"][..],
            &["--tp-listen", "0.0.0.0:48442"][..],
            &["--asic-token", "SAFA"][..],
        ] {
            let mut args = vec!["--asic-test-token", "1000"];
            args.extend(other);
            assert!(parse(&args).is_err(), "{other:?}");
        }
    }

    // #### PR #42
    // What: --merge-test-token parses with --upstream (the server refuses it
    // there without --job-declaration) and with --upstream and
    // --job-declaration together.
    // Look here if: the merge-mining flags change.
    #[test]
    fn the_merge_test_token_may_ride_job_declaration() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(
                [
                    "pickaxe",
                    "stratum-v2",
                    "serve",
                    "--sv1-listen",
                    "0.0.0.0:3333",
                ]
                .iter()
                .chain(args)
                .collect::<Vec<_>>(),
            )
        };
        let upstream = ["--upstream", "stratum2+tcp://pool.example:3336/KEY"];
        let mut args = upstream.to_vec();
        args.extend(["--merge-test-token", "1000"]);
        assert!(parse(&args).is_ok());
        args.push("--job-declaration");
        let cli = parse(&args).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Serve {
                    merge_test_token: Some(1000),
                    job_declaration: Some(JobDeclarationMode::Full),
                    ..
                }
            })
        ));
        // #### merge-mining v2: tSAFA from a deployment file, never at a
        // pool and never with the test token.
        let cli = parse(&["--safa-deployment", "tsafa.json"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Serve {
                    safa_deployment: Some(ref path),
                    ..
                }
            }) if path == std::path::Path::new("tsafa.json")
        ));
        let mut args = upstream.to_vec();
        args.extend(["--safa-deployment", "tsafa.json"]);
        assert!(parse(&args).is_err());
        assert!(parse(&["--safa-deployment", "a.json", "--merge-test-token", "1"]).is_err());
        // #### merge-mining v2: the claim broadcast delay.
        let cli = parse(&["--claim-delay", "10-20"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Serve {
                    claim_delay: Some(ref delay),
                    ..
                }
            }) if delay == "10-20"
        ));
    }

    // #### PR #42
    // What: --fallback-pool needs --sv1-listen, repeats in order, and
    // conflicts with --upstream and --public.
    // Look here if: the Serve flags change.
    #[test]
    fn fallback_pools_need_sv1_listen_and_conflict_with_upstream_and_public() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(
                ["pickaxe", "stratum-v2", "serve"]
                    .iter()
                    .chain(args)
                    .collect::<Vec<_>>(),
            )
        };
        let cli = parse(&[
            "--sv1-listen",
            "0.0.0.0:3333",
            "--fallback-pool",
            "stratum2+tcp://a.example:3336/KEY",
            "--fallback-pool",
            "stratum2+tcp://b.example:3336/KEY",
        ])
        .unwrap();
        let Some(Commands::StratumV2 {
            command: StratumV2Command::Serve { fallback_pool, .. },
        }) = cli.command
        else {
            panic!("serve options missing")
        };
        assert_eq!(fallback_pool.len(), 2);
        assert!(parse(&["--fallback-pool", "stratum2+tcp://a.example:3336/KEY"]).is_err());
        assert!(parse(&[
            "--sv1-listen",
            "0.0.0.0:3333",
            "--upstream",
            "stratum2+tcp://pool.example:3336/KEY",
            "--fallback-pool",
            "stratum2+tcp://a.example:3336/KEY",
        ])
        .is_err());
        assert!(parse(&[
            "--sv1-listen",
            "0.0.0.0:3333",
            "--public",
            "--fallback-pool",
            "stratum2+tcp://a.example:3336/KEY",
        ])
        .is_err());
    }

    // #### PR #40
    #[test]
    fn a_public_pool_takes_a_fee_from_a_chosen_source_and_cannot_also_join_a_pool() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--public",
            "--pool-fee",
            "2",
            "--pool-fee-mode",
            "both",
            "--pool-fee-address",
            "bchtest:pqpool",
        ])
        .unwrap();
        let Some(Commands::StratumV2 {
            command:
                StratumV2Command::Serve {
                    public,
                    pool_fee,
                    pool_fee_mode,
                    pool_fee_address,
                    ..
                },
        }) = cli.command
        else {
            panic!("public pool options missing")
        };
        assert!(public);
        assert_eq!(pool_fee, Some("2".parse().unwrap()));
        assert_eq!(pool_fee_mode, Some(crate::donation::bch::FeeMode::Both));
        assert_eq!(pool_fee_address.as_deref(), Some("bchtest:pqpool"));
        // Fee options need a public pool, and a public pool is not a miner
        // at someone else's pool.
        assert!(
            Cli::try_parse_from(["pickaxe", "stratum-v2", "serve", "--pool-fee", "2"]).is_err()
        );
        assert!(Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--public",
            "--upstream",
            "pool.example:3336",
            "--upstream-key",
            "key",
            "--sv1-listen",
            "0.0.0.0:3333"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--public",
            "--pool-fee-mode",
            "half"
        ])
        .is_err());
    }

    // #### PR #40
    #[test]
    fn a_server_names_its_blocks_but_a_pool_member_cannot() {
        let parse = |extra: &[&str]| {
            let mut args = vec!["pickaxe", "stratum-v2", "serve"];
            args.extend_from_slice(extra);
            Cli::try_parse_from(args)
        };
        let Some(Commands::StratumV2 {
            command: StratumV2Command::Serve { pool_tag, .. },
        }) = parse(&["--public", "--pool-tag", "/MyPool/"])
            .unwrap()
            .command
        else {
            panic!("serve options missing")
        };
        assert_eq!(pool_tag.as_deref(), Some("/MyPool/"));
        assert!(parse(&[
            "--pool-tag",
            "/MyPool/",
            "--upstream",
            "pool.example:3336",
            "--upstream-key",
            "key",
            "--sv1-listen",
            "0.0.0.0:3333"
        ])
        .is_err());
    }

    #[test]
    fn pool_mode_needs_the_pool_key_and_an_sv1_listener_and_keeps_the_donation() {
        let base = [
            "pickaxe",
            "stratum-v2",
            "serve",
            "--upstream",
            "pool.example:3336",
        ];
        let with = |extra: &[&'static str]| {
            Cli::try_parse_from(base.iter().copied().chain(extra.iter().copied()))
        };
        let cli = with(&[
            "--upstream-key",
            "9auqWEzQDVyLAAnYFbEqV2LDhYMyMEcuBJdJzkWW4GEk2Ss4Dnf",
            "--upstream-user",
            "me.rig1",
            "--sv1-listen",
            "0.0.0.0:3333",
        ])
        .unwrap();
        let Some(Commands::StratumV2 {
            command:
                StratumV2Command::Serve {
                    upstream,
                    upstream_user: Some(user),
                    ..
                },
        }) = cli.command
        else {
            panic!("pool options missing")
        };
        assert_eq!(upstream, ["pool.example:3336"]);
        assert_eq!(user, "me.rig1");
        // Backup pools repeat both options, in order.
        let cli = with(&[
            "--upstream-key",
            "key-a",
            "--upstream",
            "backup.example:3336",
            "--upstream-key",
            "key-b",
            "--sv1-listen",
            "0.0.0.0:3333",
        ])
        .unwrap();
        let Some(Commands::StratumV2 {
            command:
                StratumV2Command::Serve {
                    upstream,
                    upstream_key,
                    ..
                },
        }) = cli.command
        else {
            panic!("pool options missing")
        };
        assert_eq!(upstream, ["pool.example:3336", "backup.example:3336"]);
        assert_eq!(upstream_key, ["key-a", "key-b"]);
        // A listener for the devices is required; the pool's key may come in
        // its address, so a missing key is found when the server starts.
        assert!(with(&["--sv1-listen", "0.0.0.0:3333"]).is_ok());
        assert!(with(&["--upstream-key", "key"]).is_err());
        // #### PR #40: at a pool the donation is mining work (#### PR #48),
        // set as anywhere.
        assert!(with(&[
            "--upstream-key",
            "key",
            "--sv1-listen",
            "0.0.0.0:3333",
            "--donation",
            "2"
        ])
        .is_ok());
        // Pool identity options need a pool.
        assert!(
            Cli::try_parse_from(["pickaxe", "stratum-v2", "serve", "--upstream-user", "me"])
                .is_err()
        );
    }

    #[test]
    fn stratum_server_requires_a_valid_listener_and_preserves_global_network() {
        let cli = Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--chipnet",
            "--listen",
            "127.0.0.1:3336",
        ])
        .unwrap();
        assert!(cli.chipnet);
        assert!(matches!(
            cli.command,
            Some(Commands::StratumV2 {
                command: StratumV2Command::Serve { .. }
            })
        ));
        assert!(Cli::try_parse_from([
            "pickaxe",
            "stratum-v2",
            "serve",
            "--listen",
            "not-a-listener"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["pickaxe", "stratum-v2", "check-node", "--chipnet"]).is_ok());
    }

    // #### PR #65
    #[test]
    fn the_signed_pot_can_be_chosen_explicitly_but_not_with_another_pot() {
        assert!(Cli::try_parse_from(["pickaxe", "electric", "mine", "--signed-pot"]).is_ok());
        assert!(Cli::try_parse_from(["pickaxe", "electric", "refund", "--signed-pot"]).is_ok());
        for other in ["--restricted-pot", "--owner-pot", "--legacy-pot"] {
            assert!(
                Cli::try_parse_from(["pickaxe", "electric", "mine", "--signed-pot", other])
                    .is_err()
            );
        }
        assert!(Cli::try_parse_from([
            "pickaxe",
            "split",
            "--share",
            "electric=100",
            "--signed-electric-pot"
        ])
        .is_ok());
        for other in ["--restricted-electric-pot", "--owner-electric-pot"] {
            assert!(Cli::try_parse_from([
                "pickaxe",
                "split",
                "--share",
                "electric=100",
                "--signed-electric-pot",
                other
            ])
            .is_err());
        }
    }

    #[test]
    fn owner_pot_flags_are_explicit_and_cannot_select_a_previous_pot_too() {
        assert!(Cli::try_parse_from(["pickaxe", "electric", "wallet"]).is_ok());
        assert!(Cli::try_parse_from(["pickaxe", "electric", "pot", "--owner-pot"]).is_ok());
        assert!(Cli::try_parse_from([
            "pickaxe",
            "electric",
            "mine",
            "--owner-pot",
            "--restricted-pot"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "pickaxe",
            "electric",
            "mine",
            "--owner-pot",
            "--legacy-pot"
        ])
        .is_err());
        assert!(Cli::try_parse_from([
            "pickaxe",
            "split",
            "--share",
            "electric=100",
            "--owner-electric-pot"
        ])
        .is_ok());
        assert!(Cli::try_parse_from([
            "pickaxe",
            "split",
            "--share",
            "electric=100",
            "--owner-electric-pot",
            "--restricted-electric-pot"
        ])
        .is_err());
    }
}
