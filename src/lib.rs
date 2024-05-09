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
/// # Errors
///
/// This function will return an error if .
pub fn bye_bye_ds_stores(search_parent: &Path, recursive: &bool, verbose: &bool) -> Result<()> {
    // define pattern based on whether the user requested recursive deletion
    let pattern = match recursive {
        true => format!("{}/**/.DS_Store", search_parent.to_string_lossy()),
        false => format!("{}/.DS_Store", search_parent.to_string_lossy()),
    };

    // find all .DS_Stores
    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Finding .DS_Store files.");
    spinner.enable_steady_tick(Duration::from_millis(100));
    let hits = glob(&pattern)
        .expect("Failed to read the provided glob pattern.")
        .enumerate()
        .filter_map(|(i, hit)| {
            let message = format!("Found {} .DS_Store files", i + 1);
            spinner.set_message(message);
            hit.ok()
        })
        .collect::<Vec<PathBuf>>();
    spinner.finish();
    spinner.reset();

    // log out information about what's being searched if verbose logging is turned on
    if verbose == &true {
        let hit_count = hits.len();
        let message = match recursive {
                true => format!("Destroying {} .DS_Store files in the provided directory, {:?}, and any subdirectories.", hit_count, search_parent),
                false => format!(
                    "Destroying {} .DS_Store files in the provided directory, {:?}",
                    hit_count,
                    search_parent
                ),
            };
        eprintln!("{}", message);
    };

    // ...and destroy them (mwah-ha-ha)
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
