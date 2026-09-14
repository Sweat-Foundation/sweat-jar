use std::path::PathBuf;

#[derive(clap::Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    FetchProducts {
        #[arg(long, default_value = "test_data/products.json")]
        out: PathBuf,
    },
    BuildDb {
        #[arg(long)]
        db: PathBuf,
        #[arg(long, default_value = "test_data/interest_replay")]
        source: PathBuf,
        #[arg(long)]
        accounts: Option<PathBuf>,
        #[arg(long)]
        sample: Option<usize>,
    },
    /// Reconcile the worklist, upserting each result into the `results` table
    /// in `--db`. Resumable: an account already in `results` is skipped on a
    /// later run (so a killed process just picks up where it left off) unless
    /// `--force`. Optionally exports `results` to CSV when done — `export-csv`
    /// does the same export on demand, without recomputing anything.
    Run {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value = "test_data/products.json")]
        products: PathBuf,
        #[arg(long)]
        threads: Option<usize>,
        #[arg(long)]
        shard: Option<String>,
        #[arg(long)]
        accounts: Option<PathBuf>,
        #[arg(long)]
        sample: Option<usize>,
        #[arg(long, default_value_t = 1e-6)]
        tolerance: f64,
        /// Fetch each account's block-H state live from a NEAR archival node
        /// (jar contract `get_account` view) instead of the local `snapshots`
        /// table. Slow (~1 RPC per account) but needs no pre-populated snapshots.
        #[arg(long)]
        archival: bool,
        /// Archival JSON-RPC endpoint used when `--archival` is set.
        #[arg(long, default_value = crate::snapshot::FASTNEAR_ARCHIVAL_RPC)]
        archival_rpc_url: String,
        /// Recompute accounts that already have a `results` row instead of
        /// skipping them.
        #[arg(long)]
        force: bool,
    },
    /// Export the `results` table (populated by `run`) to a CSV file, without
    /// recomputing anything.
    ExportCsv {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Replay one account and print its per-claim breakdown vs the on-chain
    /// claim amounts (to trace where a non-zero `delta` comes from).
    Explain {
        #[arg(long)]
        db: PathBuf,
        #[arg(long)]
        account: i64,
        #[arg(long, default_value = "test_data/products.json")]
        products: PathBuf,
        #[arg(long)]
        archival: bool,
        #[arg(long, default_value = crate::snapshot::FASTNEAR_ARCHIVAL_RPC)]
        archival_rpc_url: String,
    },
}
