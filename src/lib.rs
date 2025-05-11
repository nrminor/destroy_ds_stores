#![crate_name = "dds"]

use glob::glob;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use color_eyre::eyre::Result;

pub mod cli;

#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub enum Verbosity {
    Quiet,
    #[default]
    Normal,
    Verbose,
}

impl Verbosity {
    pub fn new_from_bools(verbose: bool, quiet: bool) -> Self {
        match (verbose, quiet) {
            (true, true) => Self::Normal,
            (true, false) => Self::Verbose,
            (false, true) => Self::Quiet,
            (false, false) => Self::Normal,
        }
    }

    pub fn is_verbose(self) -> bool {
        match self {
            Verbosity::Quiet => false,
            Verbosity::Normal => false,
            Verbosity::Verbose => true,
        }
    }

    pub fn is_normal(self) -> bool {
        match self {
            Verbosity::Quiet => false,
            Verbosity::Normal => true,
            Verbosity::Verbose => false,
        }
    }

    pub fn is_quiet(self) -> bool {
        match self {
            Verbosity::Quiet => true,
            Verbosity::Normal => false,
            Verbosity::Verbose => true,
        }
    }

    pub fn is_not_quiet(self) -> bool {
        match self {
            Verbosity::Quiet => false,
            Verbosity::Normal => true,
            Verbosity::Verbose => true,
        }
    }
}

fn find_ds_stores(pattern: &str) -> Result<(Vec<PathBuf>, usize)> {
    // set up some progress logging
    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Finding .DS_Store files.");
    spinner.enable_steady_tick(Duration::from_millis(100));

    let mut hits = Vec::new();
    let mut searched = 0;

    for entry in glob(pattern)? {
        searched += 1;

        if let Ok(path) = entry {
            if path.file_name().is_some_and(|name| name == ".DS_Store") {
                hits.push(path);
            }
        }

        spinner.set_message(format!(
            "Searched {} paths, found {} .DS_Store files",
            searched,
            hits.len()
        ));

        // Implementing it this way would be much faster than constantly updating the terminal (though
        // ironically it would *feel* less fast because numbers aren't going brrrrrr)
        // ---------------------------------------------------------------------------------------
        // if searched % 100 == 0 {
        //     spinner.set_message(format!(
        //         "Searched {} paths, found {} .DS_Store files",
        //         searched,
        //         hits.len()
        //     ));
        // }
        // ---------------------------------------------------------------------------------------
    }

    // close down the spinner and clear it from the screen
    spinner.finish_and_clear();

    Ok((hits, searched))
}

pub fn bye_bye_ds_stores(
    search_parent: &Path,
    recursive: &bool,
    verbosity: Verbosity,
    dryrun: &bool,
) -> Result<()> {
    // define pattern based on whether the user requested recursive deletion
    let pattern = match recursive {
        true => format!("{}/**/.DS_Store", search_parent.to_string_lossy()),
        false => format!("{}/.DS_Store", search_parent.to_string_lossy()),
    };

    // find all .DS_Stores
    let (hits, searched_dirs) = find_ds_stores(&pattern)?;
    let num_hits = hits.len();

    // if a dry run is requested, early return
    if dryrun == &true {
        return Ok(());
    }

    // set up a pretty progress bar
    let pb = Arc::new(ProgressBar::new(hits.len() as u64));
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} .DS_Store files destroyed",
        )
        .expect("Could not set up progress bar")
        .progress_chars("=> "),
    );

    // ...otherwise, destroy the .DS_Store (mwah-ha-ha)
    hits.into_par_iter().for_each_with(pb.clone(), |pb, hit| {
        if verbosity.is_verbose() {
            eprintln!("Deleting {}", &hit.to_string_lossy());
        }
        if let Err(err) = fs::remove_file(&hit) {
            if verbosity.is_verbose() {
                eprintln!("The file at {:?} could not be deleted, either because it is read-only to this user or it no longer exists: {}", hit, err);
            }
        } else {
            pb.inc(1);
        }
    });

    pb.finish();
    let parting_message = if *recursive {
        format!(
            "{} .DS_Store files have been triumphally vanquished in {:?} and its {} subdirectories.",
            num_hits, search_parent, searched_dirs,
        )
    } else {
        format!(
            "{} .DS_Store files have been triumphally vanquished in {:?}.",
            num_hits, search_parent,
        )
    };
    eprintln!("{parting_message}");

    Ok(())
}
