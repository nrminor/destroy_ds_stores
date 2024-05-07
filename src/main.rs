use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::Result;
use dds::{bye_bye_ds_stores, cli::Cli};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let search_parent = match cli.dir.as_str() {
        "." => std::env::current_dir()?,
        _ => PathBuf::from(&cli.dir),
    };

    // check to make sure the provided search directory exists
    assert!(
        search_parent.is_dir(),
        "The provided search directory, {:?}, does not exist on the user's system or is outside of user permissions",
        search_parent
    );

    // separate out the two other runtime settings
    let recursive = &cli.recursive;
    let verbose = &cli.verbose;

    // do away with .DS_Store files based on those settings
    bye_bye_ds_stores(&search_parent, recursive, verbose)?;

    // return Ok unit-value if everything worked
    Ok(())
}
