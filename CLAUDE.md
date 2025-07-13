# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

`dds` (destroy_ds_stores) is a Rust command-line tool that finds and deletes macOS `.DS_Store` system files. It features:
- Progressive, resumable searches with persistent work queue
- Session-based search state management with interruption/resumption support
- SQLite-based caching to skip recently searched directories
- Filesystem anomaly protection (symlinks, permissions, system directories)
- Async/await architecture using Tokio for concurrent directory processing
- Parallel file deletion using rayon
- Real-time progress indicators with search statistics
- Configurable cache staleness window
- Force-refresh capability that bypasses but updates cache

## Commands

### Building
```bash
cargo build              # Debug build for development (faster compilation)
cargo build --release    # Optimized build with size optimizations (see Cargo.toml profile)
```

### Testing
```bash
cargo test
SQLX_OFFLINE=true cargo test  # Run tests in offline mode
```

### Linting and Formatting
```bash
cargo fmt        # Format code
cargo clippy     # Run linter
```

### Running
```bash
cargo run -- [OPTIONS] [DIR]
cargo run -- -r              # Recursive search in current directory
cargo run -- -d              # Dry run (find but don't delete)
cargo run -- -f              # Force refresh, bypassing cache
cargo run -- -v              # Verbose output
cargo run -- -q              # Quiet mode
cargo run -- --cache-hours 24 # Override cache window to 24 hours
cargo run -- --cache-status   # Show cache status and incomplete searches
cargo run -- --cache-stats    # Show detailed cache statistics
cargo run -- --cache-clear-incomplete  # Clear incomplete search entries
```

### Installing
```bash
cargo install --path=.
```

### Database Management
```bash
# Install sqlx-cli if needed
cargo install sqlx-cli --no-default-features --features sqlite

# For development, create a dev database
touch dev.db
export DATABASE_URL=sqlite:dev.db

# Note: The .sqlx directory is NOT committed to the repository
# For CI/CD, you'll need to either:
# 1. Run with a database available, or
# 2. Set SQLX_OFFLINE=true and handle the runtime connection
```

## Architecture

The codebase follows a modular Rust architecture with async/await and progressive caching:

### Core Modules

- `src/main.rs`: Async entry point using `#[tokio::main]`
  - Initializes color-eyre error handling
  - Sets up cancellation token for graceful shutdown (Ctrl+C)
  - Loads configuration and initializes cache
  - Handles cache management commands
  - Calls the core functionality with cache instance

- `src/lib.rs`: Core logic
  - `bye_bye_ds_stores()`: Main orchestrator function
  - `find_ds_stores_progressive()`: Progressive search with work queue
  - `process_directory_with_persistent_queue()`: Per-directory processor
  - `SearchStats`: Atomic counters for new/resumed/skipped/found/errors
  - `Verbosity` enum: Controls logging output levels
  - Filesystem anomaly detection and handling
  - Task timeout mechanism (30s per directory)

- `src/cli.rs`: Clap-based CLI argument definitions
  - Standard options: verbose, quiet, recursive, dry run
  - Cache options: force refresh, cache window override
  - Cache management: status, stats, clear-incomplete

- `src/config.rs`: Configuration management
  - Uses `dirs` crate to find home directory
  - TOML-based config at `~/.dds/config.toml`
  - Configurable database path and cache window

- `src/cache.rs`: SQLite caching and work queue system
  - Multiple tables: directory_cache, work_queue, search_sessions, found_files
  - Session-based search state (active/completed/interrupted/failed)
  - Work queue persistence for resumable searches
  - In-memory HashSet (`fresh_complete_dirs`) for O(1) lookups
  - Directory status tracking (NotCached/Incomplete/Stale/Fresh)
  - Batch operations for performance
  - Automatic cleanup of old entries

### Key Dependencies

- `clap`: Command-line argument parsing
- `tokio`: Async runtime with signal handling
- `tokio-util`: CancellationToken for graceful shutdown
- `rayon`: Parallel processing for file deletion
- `indicatif`: Progress bars and spinners
- `color-eyre`: Enhanced error handling
- `sqlx`: Compile-time checked SQL queries
- `serde` + `toml`: Configuration serialization
- `dirs`: Home directory resolution
- `chrono`: Timestamp handling
- `uuid`: Session ID generation
- `futures`: Async utilities

### Data Storage

- **Config**: `~/.dds/config.toml`
  ```toml
  cache_window_hours = 24
  database_path = "/Users/username/.dds/cache.sqlite"
  ```

- **Cache DB**: `~/.dds/cache.sqlite`
  - `directory_cache`: Tracks searched directories with timestamps
  - `work_queue`: Persistent queue of directories to process
  - `search_sessions`: Session state and metadata
  - `found_files`: .DS_Store files found per session

## Core Design Principles and Invariants

### 1. Progressive Caching Invariants
- **Search State Persistence**: All search progress MUST be persistently stored in SQLite
- **Resumability**: Any interrupted search MUST be resumable from exact stopping point
- **No Lost Work**: Work items dequeued but not processed MUST be recoverable
- **Session Integrity**: Each search session has unique ID and tracks its complete state

### 2. Cache Semantics
- **Fresh**: Directory searched within cache window (default 24h) → SKIP
- **Stale**: Directory searched outside cache window → SEARCH AGAIN
- **Incomplete**: Directory partially searched → RESUME
- **NotCached**: Never searched → SEARCH

### 3. Work Queue Management
- **Atomic Operations**: Work items must be atomically moved between states
- **No Double Processing**: Each directory processed exactly once per session
- **Peek Before Dequeue**: Check cache status before removing from queue
- **Batch Processing**: Process work in batches for performance

### 4. Filesystem Safety
- **Symlink Protection**: NEVER follow symlinks (prevent infinite loops)
- **Permission Handling**: Skip inaccessible directories gracefully
- **System Path Filtering**: Skip known problematic paths:
  - `/Volumes/` (network mounts)
  - `/.Trash` (trash folders)
  - `/System/Volumes` (system volumes)
  - `/private/var/folders` (temp folders)
  - `/.fseventsd` (file system events)
  - `/Library/Caches` (caches)
  - `/.Spotlight-V100` (Spotlight indexes)
- **Timeout Protection**: 30-second timeout per directory
- **Error Marking**: Problematic directories marked as "completed with error"

### 5. Performance Requirements
- **Concurrent Processing**: Up to 100 concurrent directory tasks
- **Batch Database Updates**: Flush every 5 seconds or when buffer full
- **In-Memory Cache**: Fresh directories cached in HashSet for O(1) lookup
- **Minimal Queue Queries**: Only query work count when items processed
- **Task Cleanup**: Remove finished tasks from tracking immediately

### 6. User Experience Invariants
- **Progress Visibility**: Always show current progress (dirs/files/queue)
- **Cancellation Response**: Ctrl+C must save state and exit gracefully
- **Dry Run Behavior**: Searches and caches but never deletes
- **Verbose Mode**: Shows detailed operations without overwhelming
- **Statistics Accuracy**: Counts must reflect actual work done

### 7. Session State Machine
```
ACTIVE → INTERRUPTED (via Ctrl+C with work remaining)
ACTIVE → COMPLETED (via normal completion or Ctrl+C with no work)
INTERRUPTED → ACTIVE (via resume)
INTERRUPTED → COMPLETED (via cleanup)
```

### 8. Force Flag Semantics
- **Cache Bypass**: Treats all directories as NotCached
- **Cache Update**: Writes fresh timestamps after search
- **No Invalidation**: Doesn't clear existing cache entries
- **Selective Refresh**: Only updates searched subtree

## Development Environment

### Nix Flake Setup
The project includes a Nix flake for reproducible development environments:

```bash
# With direnv (recommended)
direnv allow  # Automatically loads environment when entering directory

# Without direnv
nix develop   # Enter development shell manually
```

The Nix environment provides:
- Latest stable Rust toolchain with all components
- SQLite and sqlx-cli
- hyperfine for benchmarking
- All necessary build dependencies
- Pre-configured environment variables

### Rust Configuration
- `rust-toolchain.toml`: Pins Rust version to stable with required components
- `rustfmt.toml`: Code formatting rules (100 char width, crate-level imports)
- `clippy.toml`: Linting configuration with project-specific thresholds

## Development Notes

### Critical Implementation Details
- **Always use `cargo build` during development** (not release mode)
- **Work queue persistence** enables true resumability across runs
- **Session-based design** allows multiple concurrent searches
- **Batch operations** critical for performance with large directories
- **Task lifecycle**: create → track → timeout/complete → cleanup
- **Cache freshness** determined by configurable window (default 24h)

### Database Migrations
- Schema initialized on first run
- Migrations handled automatically in Cache::new()
- Indices created for common query patterns:
  - `idx_fresh_complete`: Optimizes fresh directory lookups
  - `idx_last_searched`: Optimizes time-based queries
  - `idx_work_queue_session`: Optimizes work queue operations

### Error Handling Philosophy
- **Graceful Degradation**: Cache errors don't stop main operation
- **User Visibility**: Permission/access errors shown but don't halt
- **Atomic Sessions**: Errors mark session as failed, not corrupted
- **Retry Logic**: Incomplete directories can be retried
- **Timeout Recovery**: Timed-out directories marked with errors

### Testing Strategy
- Unit tests use in-memory SQLite databases via tempfile
- Integration tests should use `--force` flag to bypass caching
- Use `SQLX_OFFLINE=true` for running tests without database setup
- Test interruption/resumption with signal simulation
- Verify cache state transitions match invariants

### Performance Considerations
- **Cold Start**: First run in large directory tree will be slow
- **Warm Cache**: Subsequent runs near-instantaneous (O(1) root check)
- **Memory Usage**: Scales with number of fresh directories in cache
- **Database Size**: Grows with directory count, auto-cleanup helps
- **Concurrent I/O**: Bottleneck is filesystem, not CPU

### Common Issues and Solutions
- **Hanging on "Queue: 0"**: Usually stuck tasks on problem directories
  - Solution: Added task timeout and cleanup
- **Session Not Resuming**: Check session status in database
  - Solution: Proper status transitions (interrupted vs completed)
- **Inflated Skip Count**: Counting all cached dirs vs session-specific
  - Solution: Session-scoped directory counting
- **Force Flag Not Working**: Cache bypass logic not complete
  - Solution: Check force_refresh in all cache methods
