#![warn(
    // clippy::pedantic,
    clippy::complexity,
    clippy::correctness,
    clippy::perf
)]
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use color_eyre::eyre::Result;
use dds::{bye_bye_ds_stores, cache::Cache, cli::Cli, config::Config, Verbosity};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    // Load config and initialize cache
    let config = Config::load().await?;
    let cache_hours = cli.cache_hours.unwrap_or(config.cache_window_hours);

    // Handle cache management commands
    if cli.cache_status {
        return handle_cache_status(&config.database_path, cache_hours).await;
    }

    if cli.cache_clear_incomplete {
        return handle_cache_clear_incomplete(&config.database_path, cache_hours).await;
    }

    if cli.cache_stats {
        return handle_cache_stats(&config.database_path, cache_hours).await;
    }

    // Normal operation - search for .DS_Store files
    let search_parent = match cli.dir.as_str() {
        "." => std::env::current_dir()?,
        _ => PathBuf::from(&cli.dir),
    };

    // check to make sure the provided search directory exists
    assert!(
        search_parent.is_dir(),
        "The provided search directory, {}, does not exist on the user's system or is outside of user permissions",
        search_parent.display()
    );

    let cache = Arc::new(Mutex::new(
        Cache::new(&config.database_path, cache_hours, cli.force).await?,
    ));

    // separate out the two other runtime settings
    let recursive = &cli.recursive;
    let dryrun = &cli.dry;
    let verbose = &cli.verbose;
    let quiet = &cli.quiet;
    let verbosity = Verbosity::new_from_bools(*verbose, *quiet);

    // Create a cancellation token
    let cancellation_token = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancellation_token.clone();

    // Set up Ctrl+C handler for graceful shutdown
    let cache_clone = Arc::clone(&cache);
    let shutdown_handle = tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {
                eprintln!("\nInterrupted! Saving search session state...");

                // Cancel the main operation
                cancel_clone.cancel();

                // Mark the current session as interrupted to enable resumption
                {
                    let mut cache_guard = cache_clone.lock().await;
                    if let Err(e) = cache_guard.interrupt_session().await {
                        eprintln!("Warning: Failed to mark session as interrupted: {e}");
                    }
                }

                eprintln!("Session state saved. You can resume this search by running the same command again.");
                std::process::exit(130); // Standard SIGINT exit code
            }
            Err(err) => {
                eprintln!("Failed to listen for Ctrl+C signal: {err}");
            }
        }
    });

    // do away with .DS_Store files based on those settings
    let result = {
        let mut cache_guard = cache.lock().await;
        bye_bye_ds_stores(
            &search_parent,
            recursive,
            verbosity,
            dryrun,
            &mut cache_guard,
            cancellation_token,
        )
        .await
    };

    // Ensure cache is dropped before returning
    drop(cache);

    // Cancel the signal handler since we're exiting normally
    shutdown_handle.abort();

    result
}

async fn handle_cache_status(database_path: &Path, cache_hours: u64) -> Result<()> {
    let cache = Cache::new(database_path, cache_hours, false).await?;
    let incomplete = cache.get_incomplete_searches().await?;

    println!("Cache Status");
    println!("============");
    println!("Database: {}", database_path.display());
    println!("Cache window: {cache_hours} hours");
    println!();

    if incomplete.is_empty() {
        println!("No incomplete searches found.");
    } else {
        println!("Incomplete searches ({} total):", incomplete.len());
        for path in incomplete {
            println!("  - {}", path.display());
        }
    }

    Ok(())
}

async fn handle_cache_clear_incomplete(database_path: &Path, cache_hours: u64) -> Result<()> {
    let cache = Cache::new(database_path, cache_hours, false).await?;
    let count = cache.clear_incomplete().await?;

    if count == 0 {
        println!("No incomplete search entries to clear.");
    } else {
        println!("Cleared {count} incomplete search entries.");
    }

    Ok(())
}

async fn handle_cache_stats(database_path: &Path, cache_hours: u64) -> Result<()> {
    let cache = Cache::new(database_path, cache_hours, false).await?;
    let stats = cache.get_cache_stats().await?;

    println!("Cache Statistics");
    println!("================");
    println!("Database: {}", database_path.display());
    println!("Cache window: {cache_hours} hours");
    println!();
    println!("Total entries:                {}", stats.total_entries);
    println!("Completed searches:           {}", stats.completed_searches);
    println!(
        "Incomplete searches:          {}",
        stats.incomplete_searches
    );
    println!(
        "Directories with .DS_Store:   {}",
        stats.directories_with_ds_store
    );
    println!(".DS_Store files deleted:      {}", stats.ds_stores_deleted);
    println!("Directories with errors:      {}", stats.errors);

    if stats.total_entries > 0 {
        let hit_rate = (stats.completed_searches as f64 / stats.total_entries as f64) * 100.0;
        println!();
        println!("Cache hit rate: {hit_rate:.1}%");
    }

    Ok(())
}
