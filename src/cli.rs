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
    /// Repeat for backup coordinators, tried in order.
    #[arg(
        long,
        global = true,
        value_name = "HOST:PORT",
        requires = "coordinator_key",
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

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum Commands {
    Mine,
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
    #[command(hide = true)]
    Repl,
}

#[derive(Subcommand, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigCommand {
    Show,
    Validate,
    Save,
}

#[derive(Subcommand, Debug, Clone, PartialEq, Eq)]
pub enum StratumV2Command {
    /// Print implementation and validation status.
    Status,
    /// Check the configured BCH node and full block template without mining.
    CheckNode,
    /// Show a running server's workers table, read-only (for example a
    /// service started with --no-tui). Use the same --config as the server.
    Watch,
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
    },
}

/// Parses command-line arguments into the supported miner commands.
pub fn parse() -> Cli {
    Cli::parse()
}

#[cfg(test)]
mod tests {
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
        assert!(
            Cli::try_parse_from(["pickaxe", "mine", "--coordinator", "192.0.2.1:3340"]).is_err()
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
        // #### PR #40: at a pool the donation is mining time, set as anywhere.
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
}
