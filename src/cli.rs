use clap::{Parser, ValueEnum};

#[derive(ValueEnum, Clone, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

/// A command line tool that deletes the `.DS_Store` system files commonly
/// found around MacOS filesystems. Please note that Finder may behave differently
/// after running `dds`.
#[derive(Parser)]
#[clap(name = "dds")]
#[clap(version = "v0.1.0")]
pub struct Cli {
    /// Control the logging of detailed information as `dds` progresses
    #[arg(short, long, value_enum, default_value_t = Verbosity::Quiet)]
    pub verbose: Verbosity,

    /// Whether to search recursively in subdirectories of the provided search directory.
    #[arg(short, long, default_value_t = false)]
    pub recursive: bool,

    /// The directory to search within for `.DS_Store` files
    #[arg(default_value = ".")]
    pub dir: String,
}
