use clap::Parser;

/// A command line tool that deletes the `.DS_Store` system files commonly
/// found around MacOS filesystems. Please note that Finder may behave differently
/// after running `dds`.
#[derive(Parser)]
#[clap(name = "dds")]
#[clap(version = "v0.2.0")]
pub struct Cli {
    /// Increase the logging of detailed information as `dds` progresses
    #[arg(short, long, default_value_t = false)]
    pub verbose: bool,

    /// Reduce the logging of detailed information as `dds` progresses
    #[arg(short, long, default_value_t = false)]
    pub quiet: bool,

    /// Whether to search recursively in subdirectories of the provided search directory.
    #[arg(short, long, default_value_t = false)]
    pub recursive: bool,

    /// Whether to perform a dry run where `.DS_Store` files are found but not deleted.
    #[arg(short, long, default_value_t = false)]
    pub dry: bool,

    /// The directory to search within for `.DS_Store` files
    #[arg(default_value = ".")]
    pub dir: String,
}
