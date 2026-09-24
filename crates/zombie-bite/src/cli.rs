use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub cmd: Commands,
}

#[derive(Subcommand, Debug, Clone)]
pub enum Commands {
    /// Bite the running network using 'doppelganger' binaries, and generate the artifacts for spawning.
    Bite {
        /// Configuration file path to use for the bite operation. CLI args override config file values.
        #[arg(long, short = 'c', verbatim_doc_comment)]
        config: Option<String>,
        /// The network will be using for bite
        /// If not specified, will use the value from config.
        /// If not in config, defaults to polkadot.
        /// The network to bite: polkadot, kusama, paseo or westend.
        /// For a relay that is not a public network use:
        /// custom%<name>%<rpc_endpoint>%<chain_spec_path>
        #[arg(short = 'r', long = "rc", verbatim_doc_comment)]
        relay: Option<String>,
        /// If provided we will override the runtime as part of the process of 'bite'
        /// The resulting network will be running with this runtime.
        #[arg(long = "rc-override", verbatim_doc_comment)]
        relay_runtime: Option<String>,
        /// Runtime to carry as an *authorized* upgrade for the relay chain.
        /// Unlike --rc-override it is NOT installed: the fork spawns on the live
        /// runtime with System::AuthorizedUpgrade seeded, so the upgrade can be
        /// enacted through the production path (apply_authorized_upgrade is
        /// permissionless), either manually or with --apply-upgrade.
        #[arg(long = "rc-upgrade", verbatim_doc_comment)]
        relay_upgrade: Option<String>,
        /// Same as --rc-upgrade but for a parachain, format: <para_id>=<wasm_path>
        /// Can be set multiple times, once per para to upgrade.
        #[arg(long = "para-upgrade", verbatim_doc_comment)]
        para_upgrade: Vec<String>,
        /// After spawn (requires --and-spawn), submit apply_authorized_upgrade
        /// for every carried upgrade and wait until it enacts.
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        apply_upgrade: bool,
        /// Keep the inherited HRMP/DMP state instead of clearing it. Only correct
        /// when the relay's parachains are exactly the ones being bitten, so the
        /// two snapshots agree on channel heads.
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        keep_messaging_state: bool,
        /// Override the cores assigned to a parachain, format: <para_id>=<cores>
        /// Can be set multiple times, once per para.
        #[arg(long = "para-cores", verbatim_doc_comment)]
        para_cores: Vec<String>,
        /// Advertise this run's own nodes as bootNodes in the published
        /// chain-specs, so the artifacts are usable by nodes this process did
        /// not start. Pass a hostname or IP to advertise (a deployment's public
        /// name); with no value the loopback addresses are published, which only
        /// works on the same host.
        #[arg(long, num_args = 0..=1, default_missing_value = "127.0.0.1", verbatim_doc_comment)]
        publish_bootnodes: Option<String>,
        /// If provided we will _bite_ the live network at the supplied block hieght
        #[arg(long = "rc-bite-at", verbatim_doc_comment)]
        relay_bite_at: Option<u32>,
        /// Parachains to include: asset-hub, coretime, people, bridge-hub, collectives (comma-separated)
        /// For custom parachains use: custom%<para_id>%<rpc_endpoint>%<chain_spec_path>%[req_cores]
        /// Example: custom:3392:wss://kusama-yap-3392.example.com:/path/to/chain-spec.json
        #[arg(long, short = 'p', value_delimiter = ',', verbatim_doc_comment)]
        parachains: Option<Vec<String>>,
        /// Base path to use. if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// sync url to use when we bite the parachain.
        #[arg(long = "rc-sync-url", verbatim_doc_comment)]
        rc_sync_url: Option<String>,
        /// Automatically spawn the 'bited' network
        #[arg(long, short = 'm', default_value_t = false, verbatim_doc_comment)]
        and_spawn: bool,
        /// Monit the progress of the chains, and restart the nodes if the block production stall
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        with_monitor: bool,
        /// Db to use
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(["rocksdb", "paritydb"]), default_value="rocksdb", verbatim_doc_comment)]
        database: String,
    },
    /// Spawn a new instance of the network from the bite step.
    Spawn {
        /// Configuration file path to use for the spawn operation. CLI args override config file values.
        #[arg(long, short = 'c', verbatim_doc_comment)]
        config: Option<String>,
        /// Base path where the 'bite' artifacts lives, we should use this base_path
        /// to find those artifacts and 'spawn' the network.
        /// if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// Monit the progress of the chains, and restart the nodes if the block prodution stall
        #[arg(long, short = 'm', default_value_t = false, verbatim_doc_comment)]
        with_monitor: bool,
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 's', value_parser = clap::builder::PossibleValuesParser::new(["spawn", "post", "after"]), default_value="spawn")]
        step: String,
        /// Submit apply_authorized_upgrade for every upgrade carried by the bite
        /// and wait until it enacts.
        #[arg(long, default_value_t = false, verbatim_doc_comment)]
        apply_upgrade: bool,
        /// Advertise this run's own nodes as bootNodes in the published
        /// chain-specs, so the artifacts are usable by nodes this process did
        /// not start. Pass a hostname or IP to advertise (a deployment's public
        /// name); with no value the loopback addresses are published, which only
        /// works on the same host.
        #[arg(long, num_args = 0..=1, default_missing_value = "127.0.0.1", verbatim_doc_comment)]
        publish_bootnodes: Option<String>,
        /// Bundle produced by 'pack' to restore into the base path before
        /// spawning, so a bite from another machine can be spawned here.
        #[arg(long, verbatim_doc_comment)]
        bundle: Option<String>,
    },
    /// Pack a step's artifacts (specs, snapshots, overrides, manifest) into a single file.
    Pack {
        /// Base path holding the artifacts.
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// Step to pack.
        #[arg(short = 's', value_parser = clap::builder::PossibleValuesParser::new(["bite", "spawn", "post"]), default_value="bite")]
        step: String,
        /// Where to write the bundle. Defaults to '<base_path>/<step>-bundle.tgz'.
        #[arg(long, short = 'o', verbatim_doc_comment)]
        out: Option<String>,
    },
    /// [Helper] Generate artifacts to be used by the next step (only 'spawn' and 'post' allowed)
    GenerateArtifacts {
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 'r', long = "rc", value_parser = clap::builder::PossibleValuesParser::new(["polkadot", "kusame", "paseo"]), default_value="polkadot")]
        relay: String,
        /// Base path to use. if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 's', value_parser = clap::builder::PossibleValuesParser::new(["spawn", "post"]), default_value="spawn")]
        step: String,
    },
    /// [Helper] Clean up directory to only include the needed artifacts
    CleanUpDir {
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 'r', long = "rc", value_parser = clap::builder::PossibleValuesParser::new(["polkadot", "kusame", "paseo"]), default_value="polkadot")]
        relay: String,
        /// Base path to use. if not provided we will check the env 'ZOMBIE_BITE_BASE_PATH' and if not present we will use `<cwd>_timestamp`
        #[arg(long, short = 'd', verbatim_doc_comment)]
        base_path: Option<String>,
        /// The network will be using for bite (will try the network + ah)
        #[arg(short = 's', value_parser = clap::builder::PossibleValuesParser::new(["bite", "spawn", "post"]), default_value="bite")]
        step: String,
    },
}
