use clap::{ArgGroup, Parser};

/// A command line tool that deletes the `.DS_Store` system files commonly
/// found around MacOS filesystems. Please note that Finder may behave differently
/// after running `dds`.
#[derive(Parser)]
#[clap(name = "dds")]
#[clap(version = "v0.2.0")]
#[clap(group(ArgGroup::new("operation")
    .args(&["cache_status", "cache_clear_incomplete", "cache_stats"])
    .conflicts_with_all(&["recursive", "dry", "force", "dir"])))]
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

    /// Force refresh, ignoring cache
    #[arg(short = 'f', long, default_value_t = false)]
    pub force: bool,

    /// Override cache window hours from config
    #[arg(long)]
    pub cache_hours: Option<u64>,

    /// Show information about incomplete searches and cache state
    #[arg(long, default_value_t = false)]
    pub cache_status: bool,

    /// Clear all incomplete search entries from the cache
    #[arg(long, default_value_t = false)]
    pub cache_clear_incomplete: bool,

    /// Show cache statistics (total entries, hit rate, etc.)
    #[arg(long, default_value_t = false)]
    pub cache_stats: bool,

    /// The directory to search within for `.DS_Store` files
    #[arg(default_value = ".")]
    pub dir: String,
}
