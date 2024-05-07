#![crate_name = "dds"]

use cli::Verbosity;
use glob::glob;
use rayon::prelude::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

use color_eyre::eyre::Result;

pub mod cli;

/// .
///
/// # Errors
///
/// This function will return an error if .
pub fn bye_bye_ds_stores(
    search_parent: &Path,
    recursive: &bool,
    verbose: &Verbosity,
) -> Result<()> {
    // log out information about what's being searched if verbose logging is turned on
    if verbose == &Verbosity::Verbose {
        let message = match recursive {
            true => format!("Destroying .DS_Store files in the current working directory, {:?}, and any subdirectories.", search_parent),
            false => format!(
                "Destroying .DS_Store files in the current working directory, {:?}",
                search_parent
            ),
        };
        eprintln!("{}", message);
    };

    // define pattern based on whether the user requested recursive deletion
    let pattern = match recursive {
        true => format!("{}/**/.DS_Store", search_parent.to_string_lossy()),
        false => format!("{}/.DS_Store", search_parent.to_string_lossy()),
    };

    // find all .DS_Stores and destroy them (mwah-ha-ha)
    glob(&pattern)
        .expect("Critical error in glob-matching encountered.")
        .flatten()
        .collect::<Vec<PathBuf>>()
        .into_par_iter()
        .for_each(|hit| {
            if let Err(err) = fs::remove_file(&hit) {
                eprintln!("Error deleting file {:?}: {}", hit, err);
            }
        });
    Ok(())
}
