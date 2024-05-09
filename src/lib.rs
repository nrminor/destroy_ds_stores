#![crate_name = "dds"]

use glob::glob;
use indicatif::ProgressBar;
use rayon::prelude::*;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use color_eyre::eyre::Result;

pub mod cli;

/// .
///
/// # Panics
///
/// Panics if .
///
/// # Errors
///
/// This function will return an error if .
fn find_ds_stores(pattern: &str) -> Result<Vec<PathBuf>> {
    // set up some progress logging
    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Finding .DS_Store files.");
    spinner.enable_steady_tick(Duration::from_millis(100));

    // collect all the hits
    let hits = glob(pattern)
        .expect("Failed to read the provided glob pattern.")
        .enumerate()
        .filter_map(|(i, hit)| {
            let message = format!("Found {} .DS_Store files", i + 1);
            spinner.set_message(message);
            hit.ok()
        })
        .collect::<Vec<PathBuf>>();

    // close down the spinner and clear it from the screen
    spinner.finish();
    spinner.reset();

    Ok(hits)
}

/// .
///
/// # Errors
///
/// This function will return an error if .
fn print_verbose_logging(hit_count: usize, recursive: &bool, search_parent: &Path) -> Result<()> {
    let message = match recursive {
            true => format!("Destroying {} .DS_Store files in the provided directory, {:?}, and any subdirectories.", hit_count, search_parent),
            false => format!(
                "Destroying {} .DS_Store files in the provided directory, {:?}",
                hit_count,
                search_parent
            ),
        };
    eprintln!("{}", message);
    Ok(())
}

/// .
///
/// # Errors
///
/// This function will return an error if .
pub fn bye_bye_ds_stores(
    search_parent: &Path,
    recursive: &bool,
    verbose: &bool,
    dryrun: &bool,
) -> Result<()> {
    // define pattern based on whether the user requested recursive deletion
    let pattern = match recursive {
        true => format!("{}/**/.DS_Store", search_parent.to_string_lossy()),
        false => format!("{}/.DS_Store", search_parent.to_string_lossy()),
    };

    // find all .DS_Stores
    let hits = find_ds_stores(&pattern)?;

    // log out information about what's being searched if verbose logging is turned on
    if verbose == &true {
        print_verbose_logging(hits.len(), recursive, search_parent)?;
    };

    // if a dry run is requested, early return
    if dryrun == &true {
        return Ok(());
    }

    // ...otherwise, destroy the .DS_Store (mwah-ha-ha)
    hits.into_par_iter().for_each(|hit| {
        if verbose == &true {
            eprintln!("Deleting {}", &hit.to_string_lossy());
        }
        if let Err(err) = fs::remove_file(&hit) {
            eprintln!("Error deleting file {:?}: {}", hit, err);
        }
    });
    Ok(())
}
