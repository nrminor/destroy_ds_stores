#![crate_name = "dds"]
#![warn(
    // clippy::pedantic,
    clippy::complexity,
    clippy::correctness,
    clippy::perf
)]

use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use rayon::prelude::*;
use regex::RegexSet;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::fs as async_fs;
use tokio_util::sync::CancellationToken;

use crate::cache::{Cache, DirectoryState, DirectoryStatus};
use color_eyre::eyre::Result;

pub mod cache;
pub mod cli;
pub mod config;

// Pre-compiled regex set for system path filtering
static SYSTEM_PATH_PATTERNS: Lazy<RegexSet> = Lazy::new(|| {
    RegexSet::new([
        r"/Volumes/",
        r"/\.Trash",
        r"/System/Volumes",
        r"/private/var/folders",
        r"/\.fseventsd",
        r"/Library/Caches",
        r"/\.Spotlight-V100",
    ])
    .expect("Failed to compile system path regex patterns")
});

/// Check if a path is a known problematic system path
#[inline]
fn is_system_path(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    SYSTEM_PATH_PATTERNS.is_match(&path_str)
}

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
            Verbosity::Verbose => false,
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

#[derive(Debug, Default)]
struct SearchStats {
    new_searches: AtomicUsize,     // Directories searched for the first time
    resumed_searches: AtomicUsize, // Directories resumed from incomplete searches
    skipped_cached: AtomicUsize,   // Directories skipped because already cached
    found: AtomicUsize,            // Total .DS_Store files found
    errors: AtomicUsize,           // Directories with errors
}

impl SearchStats {
    fn new() -> Self {
        Self::default()
    }

    fn increment_new(&self) {
        self.new_searches.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_resumed(&self) {
        self.resumed_searches.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_skipped(&self) {
        self.skipped_cached.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_found(&self) {
        self.found.fetch_add(1, Ordering::Relaxed);
    }

    fn increment_errors(&self) {
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    fn get_new(&self) -> usize {
        self.new_searches.load(Ordering::Relaxed)
    }

    fn get_resumed(&self) -> usize {
        self.resumed_searches.load(Ordering::Relaxed)
    }

    fn get_skipped(&self) -> usize {
        self.skipped_cached.load(Ordering::Relaxed)
    }

    fn get_found(&self) -> usize {
        self.found.load(Ordering::Relaxed)
    }

    fn get_errors(&self) -> usize {
        self.errors.load(Ordering::Relaxed)
    }

    fn get_total_searched(&self) -> usize {
        self.get_new() + self.get_resumed()
    }
}

async fn find_ds_stores_progressive(
    root: &Path,
    recursive: bool,
    cache: &mut Cache,
    verbosity: Verbosity,
    dry_run: bool,
    cancellation_token: CancellationToken,
) -> Result<(Vec<PathBuf>, SearchStats)> {
    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Finding .DS_Store files...");
    spinner.enable_steady_tick(Duration::from_millis(100));

    let stats = Arc::new(SearchStats::new());
    let found_files = Arc::new(Mutex::new(Vec::new()));
    let completed_dirs = Arc::new(Mutex::new(Vec::new()));
    let processing_dirs = Arc::new(Mutex::new(HashSet::new()));
    let subdirs_queue = Arc::new(Mutex::new(Vec::new()));

    // Try to resume an existing session or start a new one
    let (session_id, is_resumed) = match cache.resume_session(root, recursive, dry_run).await? {
        Some(resumed_session_id) => {
            let work_count = cache.get_work_count(&resumed_session_id).await?;
            if verbosity.is_not_quiet() {
                eprintln!(
                    "Resuming interrupted search session: {resumed_session_id} ({work_count} items in queue)"
                );
            }
            (resumed_session_id, true)
        }
        None => {
            // Start new session
            let new_session_id = cache.start_session(root, recursive, dry_run).await?;

            // Enqueue the root directory to start the search
            cache.enqueue_work(&new_session_id, root, 0).await?;

            if verbosity.is_not_quiet() {
                eprintln!("Starting new search session: {new_session_id}");
            }
            (new_session_id, false)
        }
    };

    // Load previously found files if resuming
    if is_resumed {
        let previously_found = cache.load_found_files(&session_id).await?;
        let prev_count = previously_found.len();
        found_files
            .lock()
            .expect("Failed to acquire lock on found_files")
            .extend(previously_found);
        if prev_count > 0 {
            // Update stats to reflect previously found files
            for _ in 0..prev_count {
                stats.increment_found();
            }
            if verbosity.is_not_quiet() {
                eprintln!("Loaded {prev_count} previously found .DS_Store files");
            }
        }

        // If no work remaining, this means the search was completed in previous session
        // Load the count of directories that were already searched
        let work_remaining = cache.get_work_count(&session_id).await?;
        if work_remaining == 0 {
            let searched_count = cache.get_session_searched_count(&session_id).await?;
            if searched_count > 0 {
                // Mark these directories as skipped since they were already searched
                // Use bulk increment to avoid performance issues with large counts
                stats
                    .skipped_cached
                    .fetch_add(searched_count, std::sync::atomic::Ordering::Relaxed);
                if verbosity.is_not_quiet() {
                    eprintln!(
                        "Session already completed - {searched_count} directories were previously searched"
                    );
                }
            }
        }
    }

    // Track last cache flush time
    let last_flush = Arc::new(Mutex::new(Instant::now()));
    const FLUSH_INTERVAL: Duration = Duration::from_secs(5);
    const WORK_BATCH_SIZE: usize = 50; // How many work items to process at once
    const TASK_BATCH_SIZE: usize = 100; // How many concurrent tasks to allow

    let mut tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();
    let mut total_processed = 0usize;
    let mut last_work_count = cache.get_work_count(&session_id).await?;
    let mut empty_queue_loop_count = 0usize;

    // Main work loop using persistent queue
    loop {
        // Check for cancellation
        if cancellation_token.is_cancelled() {
            eprintln!("\nSearch cancelled. Saving progress...");
            // Cancel all running tasks
            for task in tasks {
                task.abort();
            }

            // Process any final completed directories
            let final_dirs_to_update: Vec<DirectoryState> = {
                let dirs = completed_dirs
                    .lock()
                    .expect("Failed to acquire lock on completed_dirs");
                dirs.clone()
            };

            if !final_dirs_to_update.is_empty() {
                if dry_run {
                    let mut dry_run_dir_states = final_dirs_to_update;
                    for dir_state in &mut dry_run_dir_states {
                        dir_state.ds_store_deleted = false;
                    }
                    cache.mark_completed_batch(&dry_run_dir_states).await?;
                } else {
                    cache.mark_completed_batch(&final_dirs_to_update).await?;
                }
            }

            // Save found files before marking session status
            let current_found: Vec<PathBuf> = {
                let files = found_files
                    .lock()
                    .expect("Failed to acquire lock on found_files");
                files.clone()
            };
            if !current_found.is_empty() {
                cache.save_found_files(&session_id, &current_found).await?;
                if verbosity.is_verbose() {
                    eprintln!("Saved {} found files to session", current_found.len());
                }
            }

            // Check if work is actually complete
            let remaining_work = cache.get_work_count(&session_id).await?;
            if remaining_work == 0 {
                // Work is complete, mark as completed
                cache.complete_session().await?;
            } else {
                // Still work remaining, mark as interrupted
                cache.interrupt_session().await?;
            }

            // Return the progress made so far
            let found = Arc::try_unwrap(found_files)
                .map(|mutex| {
                    mutex
                        .into_inner()
                        .expect("Failed to unwrap mutex into inner value")
                })
                .unwrap_or_else(|arc| {
                    arc.lock()
                        .expect("Failed to acquire lock on found_files Arc")
                        .clone()
                });

            let final_stats = Arc::try_unwrap(stats).unwrap_or_else(|arc| SearchStats {
                new_searches: AtomicUsize::new(arc.get_new()),
                resumed_searches: AtomicUsize::new(arc.get_resumed()),
                skipped_cached: AtomicUsize::new(arc.get_skipped()),
                found: AtomicUsize::new(arc.get_found()),
                errors: AtomicUsize::new(arc.get_errors()),
            });

            return Ok((found, final_stats));
        }

        // Get work from persistent queue (peek without removing)
        let potential_work_items = cache.peek_work_batch(&session_id, WORK_BATCH_SIZE).await?;
        let work_items_empty = potential_work_items.is_empty();

        if work_items_empty && tasks.is_empty() {
            // No more work and no running tasks
            if verbosity.is_verbose() {
                eprintln!(
                    "Breaking from main loop: work_items_empty={work_items_empty}, tasks.len()={}",
                    tasks.len()
                );
            }
            break;
        }

        if verbosity.is_verbose() && work_items_empty {
            eprintln!(
                "Main loop continues: work_items_empty={work_items_empty}, tasks.len()={}",
                tasks.len()
            );
        }

        // Filter work items based on cache status and collect IDs to remove
        let mut items_to_process = Vec::new();
        let mut all_item_ids = Vec::new();

        for work_item in potential_work_items {
            // Collect ID for removal
            if let Some(id) = work_item.id {
                all_item_ids.push(id);
            }

            // Check cache status for this directory
            let dir_status = cache.get_directory_status(&work_item.path).await?;

            match dir_status {
                DirectoryStatus::Fresh => {
                    stats.increment_skipped();
                    if verbosity.is_verbose() {
                        eprintln!("Skipping cached directory: {}", work_item.path.display());
                    }
                }
                DirectoryStatus::Incomplete => {
                    stats.increment_resumed();
                    items_to_process.push(work_item);
                    if verbosity.is_verbose() {
                        eprintln!(
                            "Resuming incomplete directory: {}",
                            items_to_process
                                .last()
                                .expect("items_to_process should not be empty")
                                .path
                                .display()
                        );
                    }
                }
                DirectoryStatus::NotCached | DirectoryStatus::Stale => {
                    stats.increment_new();
                    items_to_process.push(work_item);
                    if verbosity.is_verbose() {
                        eprintln!(
                            "New directory: {}",
                            items_to_process
                                .last()
                                .expect("items_to_process should not be empty")
                                .path
                                .display()
                        );
                    }
                }
            }
        }

        // Remove all items from work queue (both processed and skipped)
        if !all_item_ids.is_empty() {
            cache.remove_work_items(&all_item_ids).await?;
        }

        // Process the items that need processing
        for work_item in items_to_process {
            // Wrap the path in Arc to avoid multiple clones
            let work_path = Arc::new(work_item.path);

            // Skip if already being processed in this run
            {
                let mut processing = processing_dirs
                    .lock()
                    .expect("Failed to acquire lock on processing_dirs");
                if processing.contains(&*work_path) {
                    continue;
                }
                processing.insert((*work_path).clone());
            }

            // Mark as searching (if not dry run)
            if !dry_run {
                cache.mark_searching(&work_path).await?;
            }

            // Wait if we have too many concurrent tasks
            while tasks.len() >= TASK_BATCH_SIZE {
                let (result, _index, remaining) = futures::future::select_all(tasks).await;
                tasks = remaining;

                if let Err(e) = result {
                    eprintln!("Task error: {e}");
                }
            }

            let stats_clone = Arc::clone(&stats);
            let found_files_clone = Arc::clone(&found_files);
            let completed_dirs_clone = Arc::clone(&completed_dirs);
            let processing_dirs_clone = Arc::clone(&processing_dirs);
            let subdirs_queue_clone = Arc::clone(&subdirs_queue);
            let session_id_clone = session_id.clone();
            let path_clone = Arc::clone(&work_path);
            let recursive_clone = recursive;

            let task = tokio::spawn(async move {
                // Add timeout to prevent hanging on problematic directories
                let result = tokio::time::timeout(
                    Duration::from_secs(30), // 30 second timeout per directory
                    process_directory_with_persistent_queue(
                        (*path_clone).clone(),
                        &session_id_clone,
                        &stats_clone,
                        &found_files_clone,
                        &completed_dirs_clone,
                        &subdirs_queue_clone,
                        recursive_clone,
                    ),
                )
                .await;

                // Remove from processing set when done
                processing_dirs_clone
                    .lock()
                    .expect("Failed to acquire lock on processing_dirs")
                    .remove(&*path_clone);

                match result {
                    Ok(inner_result) => inner_result,
                    Err(_) => {
                        eprintln!(
                            "Warning: Timeout processing directory: {}",
                            path_clone.display()
                        );
                        stats_clone.increment_errors();
                        Ok(())
                    }
                }
            });

            tasks.push(task);
            total_processed += 1;
        }

        // Update progress message - only get count if we processed items
        let work_remaining = if !all_item_ids.is_empty() {
            // Items were removed, get fresh count
            let new_count = cache.get_work_count(&session_id).await?;
            last_work_count = new_count;
            new_count
        } else if work_items_empty {
            // No items in queue, it's empty
            last_work_count = 0;
            0
        } else {
            // Use cached count when no changes
            last_work_count
        };
        let total_searched = stats.get_new() + stats.get_resumed();
        let message = if stats.get_resumed() > 0 {
            format!(
                "Searching: {new} new + {resumed} resumed = {total_searched} total | Found: {found} .DS_Store files | Skipped: {skipped} cached | Queue: {work_remaining} remaining",
                new = stats.get_new(),
                resumed = stats.get_resumed(),
                found = stats.get_found(),
                skipped = stats.get_skipped()
            )
        } else {
            format!(
                "Searching: {total_searched} directories | Found: {found} .DS_Store files | Skipped: {skipped} cached | Queue: {work_remaining} remaining",
                found = stats.get_found(),
                skipped = stats.get_skipped()
            )
        };
        spinner.set_message(message);

        // Check if we should flush cache
        let should_flush = {
            let mut last = last_flush
                .lock()
                .expect("Failed to acquire lock on last_flush");
            if last.elapsed() > FLUSH_INTERVAL {
                *last = Instant::now();
                true
            } else {
                false
            }
        };

        if should_flush {
            // Batch update completed directories
            let dirs_to_update: Vec<DirectoryState> = {
                let mut dirs = completed_dirs
                    .lock()
                    .expect("Failed to acquire lock on completed_dirs");
                dirs.drain(..).collect()
            };

            if !dirs_to_update.is_empty() {
                let count = dirs_to_update.len();
                // In dry run mode, mark as searched but not deleted
                if dry_run {
                    let mut dry_run_dir_states = dirs_to_update;
                    for dir_state in &mut dry_run_dir_states {
                        dir_state.ds_store_deleted = false;
                    }
                    cache.mark_completed_batch(&dry_run_dir_states).await?;
                } else {
                    cache.mark_completed_batch(&dirs_to_update).await?;
                }
                if verbosity.is_verbose() {
                    eprintln!("Cache flush: {count} directories marked as completed");
                }
            }

            // Force a cache flush to disk
            cache.flush_pending().await?;

            // Save found files periodically
            let current_found: Vec<PathBuf> = {
                let files = found_files
                    .lock()
                    .expect("Failed to acquire lock on found_files");
                files.clone()
            };
            if !current_found.is_empty() {
                cache.save_found_files(&session_id, &current_found).await?;
                if verbosity.is_verbose() {
                    eprintln!("Saved {} found files to session", current_found.len());
                }
            }
        }

        // Process any subdirectories that were discovered
        let subdirs_to_enqueue: Vec<(String, Vec<PathBuf>)> = {
            let mut queue = subdirs_queue
                .lock()
                .expect("Failed to acquire lock on subdirs_queue");
            queue.drain(..).collect()
        };

        for (subdir_session_id, subdirs) in subdirs_to_enqueue {
            if !subdirs.is_empty() {
                cache
                    .enqueue_work_batch(&subdir_session_id, &subdirs, 0)
                    .await?;
                if verbosity.is_verbose() {
                    eprintln!("Enqueued {} subdirectories for processing", subdirs.len());
                }
            }
        }

        // Clean up completed tasks before checking
        tasks.retain(|task| !task.is_finished());

        // Small delay to prevent busy waiting
        if work_items_empty && !tasks.is_empty() {
            // Always show diagnostics when stuck in this dir_state
            empty_queue_loop_count += 1;
            if empty_queue_loop_count % 10 == 0 {
                // Every second
                let processing_snapshot: Vec<PathBuf> = processing_dirs
                    .lock()
                    .expect("Failed to acquire lock on processing_dirs")
                    .iter()
                    .cloned()
                    .collect();
                eprintln!(
                    "\n[DEBUG] Stuck in empty queue loop for {} seconds:",
                    empty_queue_loop_count / 10
                );
                eprintln!("  - work_items_empty: {work_items_empty}");
                eprintln!("  - tasks.len(): {}", tasks.len());
                eprintln!("  - processing_dirs.len(): {}", processing_snapshot.len());

                if !processing_snapshot.is_empty() {
                    eprintln!("  - Still processing directories:");
                    for (i, path) in processing_snapshot.iter().enumerate() {
                        if i < 10 {
                            eprintln!("    {}: {}", i + 1, path.display());
                        }
                    }
                    if processing_snapshot.len() > 10 {
                        eprintln!("    ... and {} more", processing_snapshot.len() - 10);
                    }
                } else {
                    eprintln!(
                        "  - No directories in processing set (but {} tasks remain???)",
                        tasks.len()
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            // Reset the counter when we're not stuck
            empty_queue_loop_count = 0;
        }
    }

    // Wait for all remaining tasks
    if verbosity.is_verbose() && !tasks.is_empty() {
        eprintln!("Waiting for {} remaining tasks to complete...", tasks.len());
    }
    futures::future::join_all(tasks).await;
    if verbosity.is_verbose() {
        eprintln!("All tasks completed.");
    }

    // Final batch update
    let remaining_dirs: Vec<DirectoryState> = {
        let mut dirs = completed_dirs
            .lock()
            .expect("Failed to acquire lock on completed_dirs");
        dirs.drain(..).collect()
    };

    if !remaining_dirs.is_empty() {
        if dry_run {
            // In dry run mode, mark as searched but not deleted
            let mut dry_run_dir_states = remaining_dirs;
            for dir_state in &mut dry_run_dir_states {
                dir_state.ds_store_deleted = false;
            }
            cache.mark_completed_batch(&dry_run_dir_states).await?;
        } else {
            cache.mark_completed_batch(&remaining_dirs).await?;
        }
    }

    // Process any final subdirectories
    let final_subdirs: Vec<(String, Vec<PathBuf>)> = {
        let mut queue = subdirs_queue
            .lock()
            .expect("Failed to acquire lock on subdirs_queue");
        queue.drain(..).collect()
    };

    for (subdir_session_id, subdirs) in final_subdirs {
        if !subdirs.is_empty() {
            cache
                .enqueue_work_batch(&subdir_session_id, &subdirs, 0)
                .await?;
        }
    }

    // Complete the session
    cache.complete_session().await?;

    spinner.finish_and_clear();

    if verbosity.is_verbose() {
        eprintln!("Search session completed. Processed {total_processed} directories total.");
    }

    let found = Arc::try_unwrap(found_files)
        .map(|mutex| {
            mutex
                .into_inner()
                .expect("Failed to unwrap mutex into inner value")
        })
        .unwrap_or_else(|arc| {
            let mut locked = arc
                .lock()
                .expect("Failed to acquire lock on found_files Arc");
            std::mem::take(&mut *locked)
        });

    let final_stats = Arc::try_unwrap(stats).unwrap_or_else(|arc| SearchStats {
        new_searches: AtomicUsize::new(arc.get_new()),
        resumed_searches: AtomicUsize::new(arc.get_resumed()),
        skipped_cached: AtomicUsize::new(arc.get_skipped()),
        found: AtomicUsize::new(arc.get_found()),
        errors: AtomicUsize::new(arc.get_errors()),
    });

    Ok((found, final_stats))
}

type SubDirQueue = Arc<Mutex<Vec<(String, Vec<PathBuf>)>>>;
async fn process_directory_with_persistent_queue(
    dir: PathBuf,
    session_id: &str,
    stats: &Arc<SearchStats>,
    found_files: &Arc<Mutex<Vec<PathBuf>>>,
    completed_dirs: &Arc<Mutex<Vec<DirectoryState>>>,
    subdirs_queue: &SubDirQueue,
    recursive: bool,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    let mut ds_store_found = false;
    let mut error_message = None;
    let mut subdirs_to_queue = Vec::new();
    let mut search_completed = true;

    // Pre-flight checks to avoid problematic directories

    // Skip known problematic system paths
    if is_system_path(&dir) {
        error_message = Some("Skipped system/problematic directory".to_string());
        search_completed = true;
        let dir_state = DirectoryState {
            path: dir,
            last_searched_at: now,
            search_completed,
            ds_store_found: false,
            ds_store_deleted: false,
            error_message,
        };
        completed_dirs
            .lock()
            .expect("Failed to acquire lock on completed_dirs")
            .push(dir_state);
        return Ok(());
    }

    // Check if directory is accessible (permissions)
    match async_fs::metadata(&dir).await {
        Ok(metadata) => {
            // Check if it's actually a directory
            if !metadata.is_dir() {
                error_message = Some("Not a directory".to_string());
                search_completed = true; // Mark as complete so we don't retry
                let dir_state = DirectoryState {
                    path: dir,
                    last_searched_at: now,
                    search_completed,
                    ds_store_found: false,
                    ds_store_deleted: false,
                    error_message,
                };
                completed_dirs
                    .lock()
                    .expect("Failed to acquire lock on completed_dirs")
                    .push(dir_state);
                return Ok(());
            }
        }
        Err(e) => {
            // Permission denied or other access error
            error_message = Some(format!("Cannot access directory: {e}"));
            stats.increment_errors();
            search_completed = true; // Mark as complete so we don't retry
            let dir_state = DirectoryState {
                path: dir,
                last_searched_at: now,
                search_completed,
                ds_store_found: false,
                ds_store_deleted: false,
                error_message,
            };
            completed_dirs
                .lock()
                .expect("Failed to acquire lock on completed_dirs")
                .push(dir_state);
            return Ok(());
        }
    }

    // Check if it's a symlink to avoid loops
    match async_fs::symlink_metadata(&dir).await {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            // Skip symlinks to avoid infinite loops
            error_message = Some("Skipped symlink".to_string());
            search_completed = true;
            let dir_state = DirectoryState {
                path: dir,
                last_searched_at: now,
                search_completed,
                ds_store_found: false,
                ds_store_deleted: false,
                error_message,
            };
            completed_dirs
                .lock()
                .expect("Failed to acquire lock on completed_dirs")
                .push(dir_state);
            return Ok(());
        }
        _ => {} // Not a symlink or error checking, continue
    }

    // Read directory contents
    match async_fs::read_dir(&dir).await {
        Ok(mut entries) => {
            // Process each entry
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let Ok(file_type) = entry.file_type().await else {
                    continue;
                };

                if file_type.is_dir() && recursive {
                    // Skip symlinks when queueing subdirectories
                    if let Ok(metadata) = async_fs::symlink_metadata(&path).await {
                        if !metadata.file_type().is_symlink() {
                            // Also skip known problematic paths
                            if !is_system_path(&path) {
                                subdirs_to_queue.push(path);
                            }
                        }
                    }
                } else if file_type.is_file() {
                    // Check if it's a .DS_Store file
                    if let Some(name) = path.file_name() {
                        if name == ".DS_Store" {
                            found_files
                                .lock()
                                .expect("Failed to acquire lock on found_files")
                                .push(path);
                            stats.increment_found();
                            ds_store_found = true;
                        }
                    }
                }
            }
        }
        Err(e) => {
            error_message = Some(format!("Failed to read directory: {e}"));
            stats.increment_errors();
            // Mark as incomplete if we couldn't read the directory
            search_completed = false;
        }
    }

    // Store subdirectories for queueing by the main thread
    if !subdirs_to_queue.is_empty() {
        subdirs_queue
            .lock()
            .expect("Failed to acquire lock on subdirs_queue")
            .push((session_id.to_string(), subdirs_to_queue));
    }

    // Add to completed directories for batch update
    let dir_state = DirectoryState {
        path: dir,
        last_searched_at: now,
        search_completed,
        ds_store_found,
        ds_store_deleted: false, // Will be updated later when files are deleted
        error_message,
    };

    completed_dirs
        .lock()
        .expect("Failed to acquire lock on completed_dirs")
        .push(dir_state);

    Ok(())
}

pub async fn bye_bye_ds_stores(
    search_parent: &Path,
    recursive: &bool,
    verbosity: Verbosity,
    dryrun: &bool,
    cache: &mut Cache,
    cancellation_token: CancellationToken,
) -> Result<()> {
    // If this is a deletion run (not dry run), first check for any previously found but undeleted files
    let mut cached_undeleted_files = Vec::new();
    if !dryrun {
        cached_undeleted_files = cache
            .get_undeleted_ds_store_files(search_parent, *recursive)
            .await?;
        if !cached_undeleted_files.is_empty() {
            if verbosity.is_verbose() {
                eprintln!(
                    "Found {} cached .DS_Store files from previous searches that were not deleted",
                    cached_undeleted_files.len()
                );
                for file in &cached_undeleted_files {
                    eprintln!("  - {}", file.display());
                }
            } else if verbosity.is_not_quiet() {
                eprintln!(
                    "Found {} cached .DS_Store files from previous searches",
                    cached_undeleted_files.len()
                );
            }
        }
    }

    // Use the new progressive search function
    let (mut hits, stats) = find_ds_stores_progressive(
        search_parent,
        *recursive,
        cache,
        verbosity,
        *dryrun,
        cancellation_token,
    )
    .await?;

    // Add any cached undeleted files to the hits (avoiding duplicates)
    if !cached_undeleted_files.is_empty() {
        let existing_hits: std::collections::HashSet<_> = hits.iter().cloned().collect();
        for cached_file in cached_undeleted_files {
            if !existing_hits.contains(&cached_file) {
                hits.push(cached_file);
            }
        }
    }

    let num_hits = hits.len();
    let searched_dirs = stats.get_total_searched();

    // Show detailed search summary if not quiet
    if verbosity.is_not_quiet() {
        eprintln!("\nSearch Summary:");
        eprintln!("  New directories searched: {}", stats.get_new());
        if stats.get_resumed() > 0 {
            eprintln!(
                "  Resumed directories: {} (from incomplete searches)",
                stats.get_resumed()
            );
        }
        eprintln!(
            "  Skipped directories: {} (already cached)",
            stats.get_skipped()
        );
        if stats.get_errors() > 0 {
            eprintln!("  Directories with errors: {}", stats.get_errors());
        }
        eprintln!("  Total .DS_Store files found: {num_hits}");
        eprintln!();
    }

    // if a dry run is requested, early return
    if dryrun == &true {
        let parting_message = if *recursive {
            format!(
                "Dry run: {num_hits} .DS_Store files found in {} and its {searched_dirs} subdirectories.", search_parent.display()
            )
        } else {
            format!(
                "Dry run: {num_hits} .DS_Store files found in {}.",
                search_parent.display()
            )
        };
        eprintln!("{parting_message}");
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

    // Track parent directories of deleted files and files that no longer exist
    let deleted_parents: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let missing_parents: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));

    // ...otherwise, destroy the .DS_Store (mwah-ha-ha)
    hits.into_par_iter().for_each_with(
        (pb.clone(), deleted_parents.clone(), missing_parents.clone()),
        |(pb, deleted, missing), hit| {
            if verbosity.is_verbose() {
                eprintln!("Deleting {}", &hit.to_string_lossy());
            }
            match fs::remove_file(&hit) {
                Ok(()) => {
                    pb.inc(1);
                    if let Some(parent) = hit.parent() {
                        deleted
                            .lock()
                            .expect("Failed to acquire lock on deleted_parents")
                            .insert(parent.to_path_buf());
                    }
                }
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::NotFound {
                        // File was already deleted (perhaps manually)
                        if verbosity.is_verbose() {
                            eprintln!("File no longer exists: {}", &hit.to_string_lossy());
                        }
                        // Still mark the parent directory as having its .DS_Store deleted
                        if let Some(parent) = hit.parent() {
                            missing
                                .lock()
                                .expect("Failed to acquire lock on missing_parents")
                                .insert(parent.to_path_buf());
                        }
                    } else if verbosity.is_verbose() {
                        eprintln!(
                            "The file at {} could not be deleted due to permissions: {err}",
                            hit.display()
                        );
                    }
                }
            }
        },
    );

    pb.finish();

    // Mark directories where we deleted files as completed with ds_store_deleted = true
    let mut all_affected_dirs = deleted_parents
        .lock()
        .expect("Failed to acquire lock on deleted_parents")
        .clone();
    all_affected_dirs.extend(
        missing_parents
            .lock()
            .expect("Failed to acquire lock on missing_parents")
            .iter()
            .cloned(),
    );

    let dirs_to_mark: Vec<PathBuf> = all_affected_dirs.into_iter().collect();
    let dir_states_to_mark: Vec<DirectoryState> = dirs_to_mark
        .into_iter()
        .map(|dir| DirectoryState {
            path: dir,
            last_searched_at: chrono::Utc::now().timestamp(),
            search_completed: true,
            ds_store_found: true,
            ds_store_deleted: true,
            error_message: None,
        })
        .collect();

    // Only update cache if not a dry run
    if !dir_states_to_mark.is_empty() && !dryrun {
        cache.mark_completed_batch(&dir_states_to_mark).await?;
    }

    // Optionally cleanup old entries
    if num_hits > 0 {
        let _ = cache.cleanup_old_entries().await;
    }

    let parting_message = if *recursive {
        format!(
            "{num_hits} .DS_Store files have been triumphally vanquished in {} and its {searched_dirs} subdirectories.", search_parent.display(),
        )
    } else {
        format!(
            "{num_hits} .DS_Store files have been triumphally vanquished in {}.",
            search_parent.display()
        )
    };
    eprintln!("{parting_message}");

    Ok(())
}
