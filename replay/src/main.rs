use std::collections::HashSet;

use clap::Parser;

use replay::{cli, db, explain, export, products, run};

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    match cli.cmd {
        cli::Cmd::FetchProducts { out } => {
            let n = products::fetch_products(products::MAINNET_RPC, products::JAR_CONTRACT, &out)?;
            println!("wrote {n} products to {}", out.display());
            Ok(())
        }
        cli::Cmd::BuildDb {
            db: db_path,
            source,
            accounts,
            sample,
            memory_limit,
        } => {
            let accounts: Option<HashSet<i64>> = match accounts {
                Some(path) => Some(db::ingest::read_accounts(&path)?.into_iter().collect()),
                None => None,
            };
            let mut conn = db::open_write(&db_path)?;
            // build-db's three INSERT..SELECT batches run silent otherwise --
            // this is the only progress signal for a job that takes minutes.
            conn.execute_batch("PRAGMA enable_progress_bar; PRAGMA progress_bar_time=1000;")?;
            // Spill to disk next to the db file rather than exhausting host
            // RAM on the `events` insert (the join+sort over the full export).
            let temp_dir = db_path.with_extension("tmp");
            conn.execute_batch(&format!(
                "PRAGMA temp_directory='{}';",
                temp_dir.display()
            ))?;
            if let Some(limit) = &memory_limit {
                conn.execute_batch(&format!("PRAGMA memory_limit='{limit}';"))?;
            }
            db::schema::init_schema(&conn)?;
            let counts = db::ingest::build_db(
                &mut conn,
                &db::ingest::BuildOpts {
                    source_dir: &source,
                    accounts: accounts.as_ref(),
                    sample,
                },
            )?;
            for (table, n) in counts {
                println!("{table}: {n}");
            }
            Ok(())
        }
        cli::Cmd::Run {
            db,
            out,
            products,
            threads,
            shard,
            accounts,
            sample,
            tolerance,
            archival,
            archival_rpc_url,
            force,
        } => {
            let threads = threads.unwrap_or_else(|| {
                std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
            });
            let shard = shard.as_deref().map(run::parse_shard).transpose()?;
            let summary = run::run(&run::RunOpts {
                db,
                out,
                products,
                threads,
                shard,
                accounts,
                sample,
                tolerance,
                archival_rpc_url: archival.then_some(archival_rpc_url),
                force,
            })?;
            println!(
                "processed {} | ok {} | error {} | failure {} | no_baseline {} | over_tolerance {}",
                summary.processed,
                summary.ok,
                summary.errored,
                summary.failed,
                summary.no_baseline,
                summary.over_tolerance
            );
            println!(
                "sum_calculated {} | sum_actual {}",
                summary.sum_calculated, summary.sum_actual
            );
            Ok(())
        }
        cli::Cmd::ExportCsv { db, out } => {
            export::export_csv(&db, &out)?;
            println!("wrote {}", out.display());
            Ok(())
        }
        cli::Cmd::Explain {
            db,
            account,
            products,
            archival,
            archival_rpc_url,
        } => explain::explain(&explain::ExplainOpts {
            db,
            account,
            products,
            archival_rpc_url: archival.then_some(archival_rpc_url),
        }),
    }
}
