use anyhow::Result;
use chronicle::stream::etl::{Refinery, SymbolCatalog};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "chronicle-etl")]
#[command(about = "Universal Ingest Gateway for Chronicle")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Extract data to Parquet (ML/Analysis)
    Extract {
        /// Input Raw Queue path
        #[arg(long)]
        source: PathBuf,

        /// Output Parquet file path
        #[arg(long)]
        output: PathBuf,
    },
    /// Refine raw data into clean stream (Live/Storage)
    Refine {
        /// Input Raw Queue path
        #[arg(long)]
        source: PathBuf,

        /// Reader name for the source queue
        #[arg(long, default_value = "etl_refinery")]
        reader: String,

        /// Output Clean Queue path
        #[arg(long)]
        sink: PathBuf,

        /// Snapshot injection interval in seconds
        #[arg(long, default_value_t = 60)]
        interval: u64,

        /// Delta Lake dim_symbol table path
        #[arg(long)]
        catalog_delta: Option<PathBuf>,

        /// Optional catalog refresh interval in seconds (0 disables refresh)
        #[arg(long, default_value_t = 0)]
        catalog_refresh_secs: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Extract { source, output } => {
            println!("Extracting from {:?} to {:?}", source, output);
            // TODO: Wiring up the complex Extractor logic requires constructing
            // FeatureSet, ReplayEngine, and ParquetSink.
            // For this phase, we focus on Refine.
            println!("Extraction feature pending integration in main.rs");
        }
        Commands::Refine {
            source,
            reader,
            sink,
            interval,
            catalog_delta,
            catalog_refresh_secs,
        } => {
            println!("Refining {:?} -> {:?}", source, sink);
            let catalog_path = catalog_delta.unwrap_or_else(|| default_catalog_path());
            let catalog = SymbolCatalog::load_delta(&catalog_path)?;
            let catalog = Arc::new(RwLock::new(catalog));

            if catalog_refresh_secs > 0 {
                let refresh_path = catalog_path.clone();
                let refresh_catalog = Arc::clone(&catalog);
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(catalog_refresh_secs));
                    match SymbolCatalog::load_delta(&refresh_path) {
                        Ok(next) => {
                            if let Ok(mut guard) = refresh_catalog.write() {
                                *guard = next;
                            }
                        }
                        Err(err) => {
                            eprintln!("catalog refresh failed: {err}");
                        }
                    }
                });
            }

            let mut refinery = Refinery::new(
                source,
                &reader,
                sink,
                Duration::from_secs(interval),
                catalog,
            )?;
            refinery.run()?;
        }
    }

    Ok(())
}

fn default_catalog_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join("data")
        .join("lake")
        .join("silver")
        .join("dim_symbol")
}
