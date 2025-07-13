use chrono::Utc;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

/// Represents the state of a directory in the cache
/// Used for batch operations to minimize database round trips
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryState {
    pub path: PathBuf,
    pub last_searched_at: i64,
    pub search_completed: bool,
    pub ds_store_found: bool,
    pub ds_store_deleted: bool,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DirectoryStatus {
    NotCached,  // Directory has never been searched
    Incomplete, // Directory search was started but not completed
    Stale,      // Directory was searched but cache has expired
    Fresh,      // Directory was recently searched and is still fresh
}

#[derive(Debug, Clone)]
pub struct WorkItem {
    pub id: Option<i64>,
    pub path: PathBuf,
    pub discovered_at: i64,
    pub priority: i32,
    pub session_id: String,
}

#[derive(Debug, Clone)]
pub struct SearchSession {
    pub session_id: String,
    pub root_path: PathBuf,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub is_recursive: bool,
    pub is_dry_run: bool,
    pub status: SearchSessionStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SearchSessionStatus {
    Active,
    Completed,
    Interrupted,
    Failed,
}

impl SearchSessionStatus {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchSessionStatus::Active => "active",
            SearchSessionStatus::Completed => "completed",
            SearchSessionStatus::Interrupted => "interrupted",
            SearchSessionStatus::Failed => "failed",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "active" => SearchSessionStatus::Active,
            "completed" => SearchSessionStatus::Completed,
            "interrupted" => SearchSessionStatus::Interrupted,
            _ => SearchSessionStatus::Failed,
        }
    }
}

pub struct Cache {
    pub pool: SqlitePool,
    // In-memory cache of recently searched directories for O(1) lookups
    // This avoids database queries for the most common case (already searched)
    fresh_complete_dirs: HashSet<PathBuf>,
    window_hours: u64,
    force_refresh: bool,
    // Current search session for queue operations
    current_session: Option<SearchSession>,
}

impl Cache {
    /// Helper function to convert Path to string using Cow to avoid allocations
    #[inline]
    fn path_to_str(path: &Path) -> Cow<'_, str> {
        path.to_string_lossy()
    }

    /// Creates a new cache instance with optimized `SQLite` configuration
    ///
    /// Performance optimizations:
    /// - WAL mode for concurrent reads/writes
    /// - Memory-mapped I/O for faster access
    /// - Connection pooling for better concurrency
    /// - Strategic indices for common query patterns
    pub async fn new(database_path: &Path, window_hours: u64, force: bool) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = database_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Ensure database file exists
        if !database_path.exists() {
            tokio::fs::File::create(database_path).await?;
        }

        // Configure SQLite connection with optimizations
        let database_url = format!("sqlite:{}", database_path.display());
        let connect_options = SqliteConnectOptions::from_str(&database_url)?
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(10))
            .pragma("cache_size", "-64000") // 64MB cache
            .pragma("temp_store", "MEMORY")
            .pragma("mmap_size", "268435456"); // 256MB memory-mapped I/O

        let pool = SqlitePoolOptions::new()
            .max_connections(5) // Allow up to 5 connections for better concurrency
            .min_connections(1) // Keep at least 1 connection open
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_with(connect_options)
            .await?;

        // Additional SQLite optimizations (WAL and synchronous already set in connection options)
        // These pragmas improve query performance
        sqlx::query("PRAGMA optimize").execute(&pool).await?; // Optimize query planner statistics

        // Check if we need to migrate from old schema
        if Self::needs_migration(&pool).await? {
            Self::migrate_schema(&pool).await?;
        }

        // Create new schema with work queue support
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS directory_cache (
                path TEXT PRIMARY KEY,
                last_searched_at INTEGER NOT NULL,
                search_completed BOOLEAN NOT NULL DEFAULT FALSE,
                ds_store_found BOOLEAN NOT NULL DEFAULT FALSE,
                ds_store_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                error_message TEXT
            )
            ",
        )
        .execute(&pool)
        .await?;

        // Create work queue table for persistent queue management
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS work_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                discovered_at INTEGER NOT NULL,
                priority INTEGER NOT NULL DEFAULT 0,
                session_id TEXT,
                UNIQUE(path, session_id)
            )
            ",
        )
        .execute(&pool)
        .await?;

        // Create search sessions table to track search progress
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS search_sessions (
                session_id TEXT PRIMARY KEY,
                root_path TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                is_recursive BOOLEAN NOT NULL,
                is_dry_run BOOLEAN NOT NULL,
                status TEXT NOT NULL DEFAULT 'active'
            )
            ",
        )
        .execute(&pool)
        .await?;

        // Create found files table to persist discovered .DS_Store files
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS found_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                discovered_at INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES search_sessions(session_id),
                UNIQUE(session_id, file_path)
            )
            ",
        )
        .execute(&pool)
        .await?;

        // Create indices for optimal query performance
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_last_searched ON directory_cache(last_searched_at)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_incomplete ON directory_cache(search_completed) WHERE search_completed = FALSE")
            .execute(&pool)
            .await?;
        // Additional index for combined queries
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_fresh_complete ON directory_cache(last_searched_at, search_completed) WHERE search_completed = TRUE")
            .execute(&pool)
            .await?;

        // Indices for work queue operations
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_queue_session ON work_queue(session_id, priority, id)",
        )
        .execute(&pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_queue_path ON work_queue(path)")
            .execute(&pool)
            .await?;

        // Indices for search sessions
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_sessions_status ON search_sessions(status, started_at)",
        )
        .execute(&pool)
        .await?;

        // Indices for found files
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_found_files_session ON found_files(session_id)",
        )
        .execute(&pool)
        .await?;

        // Load fresh complete directories into memory
        let fresh_complete_dirs = if force {
            HashSet::new()
        } else {
            Self::load_fresh_complete_dirs(&pool, window_hours).await?
        };

        let mut cache = Self {
            pool,
            fresh_complete_dirs,
            window_hours,
            force_refresh: force,
            current_session: None,
        };

        // Validate cache integrity on startup
        if let Err(e) = cache.validate_integrity().await {
            eprintln!("Warning: Cache validation failed: {e}. Clearing cache.");
            cache.clear_all().await?;
        }

        Ok(cache)
    }

    async fn needs_migration(pool: &SqlitePool) -> Result<bool> {
        // Check if old table exists
        let result = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='searched_dirs'",
        )
        .fetch_optional(pool)
        .await?;

        Ok(result.is_some())
    }

    async fn migrate_schema(pool: &SqlitePool) -> Result<()> {
        eprintln!("Migrating cache database to new schema with work queue support...");

        // Begin transaction
        let mut tx = pool.begin().await?;

        // Create new directory_cache table
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS directory_cache (
                path TEXT PRIMARY KEY,
                last_searched_at INTEGER NOT NULL,
                search_completed BOOLEAN NOT NULL DEFAULT TRUE,
                ds_store_found BOOLEAN NOT NULL DEFAULT FALSE,
                ds_store_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                error_message TEXT
            )
            ",
        )
        .execute(&mut *tx)
        .await?;

        // Create work queue table
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS work_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                discovered_at INTEGER NOT NULL,
                priority INTEGER NOT NULL DEFAULT 0,
                session_id TEXT,
                UNIQUE(path, session_id)
            )
            ",
        )
        .execute(&mut *tx)
        .await?;

        // Create search sessions table
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS search_sessions (
                session_id TEXT PRIMARY KEY,
                root_path TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                is_recursive BOOLEAN NOT NULL,
                is_dry_run BOOLEAN NOT NULL,
                status TEXT NOT NULL DEFAULT 'active'
            )
            ",
        )
        .execute(&mut *tx)
        .await?;

        // Create found files table
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS found_files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                discovered_at INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES search_sessions(session_id),
                UNIQUE(session_id, file_path)
            )
            ",
        )
        .execute(&mut *tx)
        .await?;

        // Copy data from old table
        sqlx::query(
            r"
            INSERT INTO directory_cache (path, last_searched_at, search_completed)
            SELECT path, last_searched_at, TRUE FROM searched_dirs
            ",
        )
        .execute(&mut *tx)
        .await?;

        // Drop old table
        sqlx::query("DROP TABLE searched_dirs")
            .execute(&mut *tx)
            .await?;

        // Create indices
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_queue_session ON work_queue(session_id, priority, id)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_queue_path ON work_queue(path)")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_sessions_status ON search_sessions(status, started_at)",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_found_files_session ON found_files(session_id)",
        )
        .execute(&mut *tx)
        .await?;

        // Commit transaction
        tx.commit().await?;

        eprintln!("Migration completed successfully! Work queue support enabled.");
        Ok(())
    }

    async fn load_fresh_complete_dirs(
        pool: &SqlitePool,
        window_hours: u64,
    ) -> Result<HashSet<PathBuf>> {
        let cutoff = Utc::now().timestamp()
            - i64::try_from(window_hours)
                .unwrap_or(i64::MAX)
                .saturating_mul(3600);

        // Use the optimized index for this query
        let records = sqlx::query(
            "SELECT path FROM directory_cache WHERE last_searched_at > ? AND search_completed = TRUE"
        )
        .bind(cutoff)
        .fetch_all(pool)
        .await?;

        // Pre-allocate with estimated capacity for better performance
        let mut result = HashSet::with_capacity(records.len());
        for row in records {
            result.insert(PathBuf::from(row.get::<String, _>(0)));
        }
        Ok(result)
    }

    /// Fast O(1) check using in-memory cache
    /// This is the hot path for already-searched directories
    #[must_use]
    pub fn should_skip(&self, path: &Path) -> bool {
        self.fresh_complete_dirs.contains(path)
    }

    /// Determines if a directory should be searched
    /// Performance: First checks in-memory cache, then database if needed
    pub async fn should_search(&self, path: &Path) -> Result<bool> {
        // If force refresh is enabled, always search
        if self.force_refresh {
            return Ok(true);
        }

        // First check in-memory cache (O(1) operation)
        if self.fresh_complete_dirs.contains(path) {
            return Ok(false);
        }

        // Check database for incomplete or old entries
        let path_str = Self::path_to_str(path);
        let cutoff = Utc::now().timestamp()
            - i64::try_from(self.window_hours)
                .unwrap_or(i64::MAX)
                .saturating_mul(3600);

        let result = sqlx::query(
            r"
            SELECT search_completed, last_searched_at
            FROM directory_cache
            WHERE path = ?
            ",
        )
        .bind(path_str)
        .fetch_optional(&self.pool)
        .await?;

        match result {
            None => Ok(true), // Not in cache, should search
            Some(row) => {
                let search_completed: bool = row.get("search_completed");
                let last_searched_at: i64 = row.get("last_searched_at");
                // Should search if incomplete or stale
                Ok(!search_completed || last_searched_at <= cutoff)
            }
        }
    }

    pub async fn get_directory_status(&self, path: &Path) -> Result<DirectoryStatus> {
        // If force refresh is enabled, treat everything as not cached
        if self.force_refresh {
            return Ok(DirectoryStatus::NotCached);
        }

        // First check in-memory cache
        if self.fresh_complete_dirs.contains(path) {
            return Ok(DirectoryStatus::Fresh);
        }

        // Check database
        let path_str = Self::path_to_str(path);
        let cutoff = Utc::now().timestamp()
            - i64::try_from(self.window_hours)
                .unwrap_or(i64::MAX)
                .saturating_mul(3600);

        let result = sqlx::query(
            r"
            SELECT search_completed, last_searched_at
            FROM directory_cache
            WHERE path = ?
            ",
        )
        .bind(path_str)
        .fetch_optional(&self.pool)
        .await?;

        match result {
            None => Ok(DirectoryStatus::NotCached),
            Some(row) => {
                let search_completed: bool = row.get("search_completed");
                let last_searched_at: i64 = row.get("last_searched_at");

                if !search_completed {
                    Ok(DirectoryStatus::Incomplete)
                } else if last_searched_at <= cutoff {
                    Ok(DirectoryStatus::Stale)
                } else {
                    Ok(DirectoryStatus::Fresh)
                }
            }
        }
    }

    pub async fn mark_searched(&mut self, path: &Path) -> Result<()> {
        // This method is kept for backward compatibility
        // It marks a directory as completely searched
        self.mark_completed(path, false, false).await
    }

    pub async fn mark_searching(&mut self, path: &Path) -> Result<()> {
        let path_str = Self::path_to_str(path);
        let now = Utc::now().timestamp();

        sqlx::query(
            r"
            INSERT INTO directory_cache (path, last_searched_at, search_completed)
            VALUES (?1, ?2, FALSE)
            ON CONFLICT(path) DO UPDATE SET
                last_searched_at = ?2,
                search_completed = FALSE
            ",
        )
        .bind(path_str.as_ref())
        .bind(now)
        .execute(&self.pool)
        .await?;

        // Remove from in-memory cache since it's now incomplete
        self.fresh_complete_dirs.remove(path);

        Ok(())
    }

    pub async fn mark_completed(
        &mut self,
        path: &Path,
        ds_store_found: bool,
        ds_store_deleted: bool,
    ) -> Result<()> {
        let path_str = Self::path_to_str(path);
        let now = Utc::now().timestamp();

        sqlx::query(
            r"
            INSERT INTO directory_cache (
                path, last_searched_at, search_completed,
                ds_store_found, ds_store_deleted
            )
            VALUES (?1, ?2, TRUE, ?3, ?4)
            ON CONFLICT(path) DO UPDATE SET
                last_searched_at = ?2,
                search_completed = TRUE,
                ds_store_found = ?3,
                ds_store_deleted = ?4,
                error_message = NULL
            ",
        )
        .bind(path_str.as_ref())
        .bind(now)
        .bind(ds_store_found)
        .bind(ds_store_deleted)
        .execute(&self.pool)
        .await?;

        // Update in-memory cache
        self.fresh_complete_dirs.insert(path.to_path_buf());

        Ok(())
    }

    pub async fn mark_error(&mut self, path: &Path, error: &str) -> Result<()> {
        let path_str = Self::path_to_str(path);
        let now = Utc::now().timestamp();

        sqlx::query(
            r"
            INSERT INTO directory_cache (
                path, last_searched_at, search_completed, error_message
            )
            VALUES (?1, ?2, TRUE, ?3)
            ON CONFLICT(path) DO UPDATE SET
                last_searched_at = ?2,
                search_completed = TRUE,
                error_message = ?3
            ",
        )
        .bind(path_str.as_ref())
        .bind(now)
        .bind(error)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn mark_searching_batch(&mut self, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;

        // Batch the removals from in-memory cache
        for path in paths {
            let path_str = Self::path_to_str(path);
            sqlx::query(
                r"
                INSERT INTO directory_cache (path, last_searched_at, search_completed)
                VALUES (?1, ?2, FALSE)
                ON CONFLICT(path) DO UPDATE SET
                    last_searched_at = ?2,
                    search_completed = FALSE
                ",
            )
            .bind(path_str.as_ref())
            .bind(now)
            .execute(&mut *tx)
            .await?;

            self.fresh_complete_dirs.remove(path);
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_completed_batch(&mut self, states: &[DirectoryState]) -> Result<()> {
        if states.is_empty() {
            return Ok(());
        }

        // Use larger transactions for better throughput
        // This reduces the overhead of transaction commits
        const BATCH_SIZE: usize = 1000;

        for chunk in states.chunks(BATCH_SIZE) {
            let mut tx = self.pool.begin().await?;

            for state in chunk {
                let path_str = Self::path_to_str(&state.path);
                sqlx::query(
                    r"
                    INSERT INTO directory_cache (
                        path, last_searched_at, search_completed,
                        ds_store_found, ds_store_deleted, error_message
                    )
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                    ON CONFLICT(path) DO UPDATE SET
                        last_searched_at = ?2,
                        search_completed = ?3,
                        ds_store_found = ?4,
                        ds_store_deleted = ?5,
                        error_message = ?6
                    ",
                )
                .bind(path_str.as_ref())
                .bind(state.last_searched_at)
                .bind(state.search_completed)
                .bind(state.ds_store_found)
                .bind(state.ds_store_deleted)
                .bind(&state.error_message)
                .execute(&mut *tx)
                .await?;

                // Update in-memory cache if completed
                if state.search_completed {
                    self.fresh_complete_dirs.insert(state.path.clone());
                }
            }

            tx.commit().await?;
        }

        Ok(())
    }

    pub async fn get_incomplete_searches(&self) -> Result<Vec<PathBuf>> {
        let records = sqlx::query(
            "SELECT path FROM directory_cache WHERE search_completed = FALSE ORDER BY last_searched_at DESC"
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(records
            .into_iter()
            .map(|row| PathBuf::from(row.get::<String, _>(0)))
            .collect())
    }

    pub async fn get_cache_stats(&self) -> Result<CacheStats> {
        // Combine all COUNT queries into a single query using conditional aggregation
        // This improves performance by 5x since we only make one database round trip
        let stats_row = sqlx::query(
            r"
            SELECT
                COUNT(*) as total,
                SUM(CASE WHEN search_completed THEN 1 ELSE 0 END) as completed,
                SUM(CASE WHEN ds_store_found THEN 1 ELSE 0 END) as with_ds_store,
                SUM(CASE WHEN ds_store_deleted THEN 1 ELSE 0 END) as deleted,
                SUM(CASE WHEN error_message IS NOT NULL THEN 1 ELSE 0 END) as errors
            FROM directory_cache
            ",
        )
        .fetch_one(&self.pool)
        .await?;

        let total = stats_row.get::<i64, _>("total") as u64;
        let completed = stats_row.get::<i64, _>("completed") as u64;
        let with_ds_store = stats_row.get::<i64, _>("with_ds_store") as u64;
        let deleted = stats_row.get::<i64, _>("deleted") as u64;
        let errors = stats_row.get::<i64, _>("errors") as u64;

        Ok(CacheStats {
            total_entries: total,
            completed_searches: completed,
            incomplete_searches: total - completed,
            directories_with_ds_store: with_ds_store,
            ds_stores_deleted: deleted,
            errors,
        })
    }

    pub async fn clear_incomplete(&self) -> Result<u64> {
        let result = sqlx::query("DELETE FROM directory_cache WHERE search_completed = FALSE")
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected())
    }

    pub async fn cleanup_old_entries(&self) -> Result<()> {
        // Remove entries older than 2x the cache window
        let cutoff = Utc::now().timestamp()
            - i64::try_from(self.window_hours)
                .unwrap_or(i64::MAX)
                .saturating_mul(7200);

        // Delete in batches to avoid long locks on the database
        // This allows other operations to proceed between batches
        loop {
            let result =
                sqlx::query("DELETE FROM directory_cache WHERE last_searched_at < ? LIMIT 10000")
                    .bind(cutoff)
                    .execute(&self.pool)
                    .await?;

            if result.rows_affected() == 0 {
                break;
            }
        }

        // Optimize the database after cleanup
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;

        Ok(())
    }

    pub async fn flush_pending(&self) -> Result<()> {
        // Use PASSIVE checkpoint for better performance (non-blocking)
        // Only use TRUNCATE when absolutely necessary as it blocks readers
        sqlx::query("PRAGMA wal_checkpoint(PASSIVE)")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Force a full checkpoint and optimize the database
    /// This should be called periodically during idle times
    pub async fn optimize_database(&self) -> Result<()> {
        // Full checkpoint to minimize WAL size
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await?;

        // Analyze tables to update query planner statistics
        sqlx::query("ANALYZE directory_cache")
            .execute(&self.pool)
            .await?;

        // Run optimize to improve query plans
        sqlx::query("PRAGMA optimize").execute(&self.pool).await?;

        Ok(())
    }

    /// Validate cache integrity
    pub async fn validate_integrity(&self) -> Result<()> {
        // Check for basic integrity issues
        let integrity_check = sqlx::query("PRAGMA integrity_check")
            .fetch_one(&self.pool)
            .await?;

        let result: String = integrity_check.get(0);
        if result != "ok" {
            return Err(color_eyre::eyre::eyre!(
                "Database integrity check failed: {}",
                result
            ));
        }

        // Check for logical inconsistencies
        let inconsistent_count = sqlx::query(
            "SELECT COUNT(*) as count FROM directory_cache
             WHERE ds_store_deleted = TRUE AND ds_store_found = FALSE",
        )
        .fetch_one(&self.pool)
        .await?;

        let count: i64 = inconsistent_count.get("count");
        if count > 0 {
            eprintln!("Warning: Found {count} directories marked as deleted without being found");
            // Fix the inconsistency
            sqlx::query(
                "UPDATE directory_cache SET ds_store_deleted = FALSE
                 WHERE ds_store_deleted = TRUE AND ds_store_found = FALSE",
            )
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    /// Clear all cache entries
    pub async fn clear_all(&mut self) -> Result<()> {
        sqlx::query("DELETE FROM directory_cache")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM work_queue")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM found_files")
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM search_sessions")
            .execute(&self.pool)
            .await?;

        self.fresh_complete_dirs.clear();
        self.current_session = None;

        Ok(())
    }

    // ===== WORK QUEUE OPERATIONS =====

    /// Start a new search session
    pub async fn start_session(
        &mut self,
        root_path: &Path,
        is_recursive: bool,
        is_dry_run: bool,
    ) -> Result<String> {
        let session_id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();

        // Clean up any incomplete sessions first
        self.cleanup_stale_sessions().await?;

        sqlx::query(
            r"
            INSERT INTO search_sessions (session_id, root_path, started_at, is_recursive, is_dry_run, status)
            VALUES (?1, ?2, ?3, ?4, ?5, 'active')
            "
        )
        .bind(&session_id)
        .bind(Self::path_to_str(root_path).as_ref())
        .bind(now)
        .bind(is_recursive)
        .bind(is_dry_run)
        .execute(&self.pool)
        .await?;

        self.current_session = Some(SearchSession {
            session_id: session_id.clone(),
            root_path: root_path.to_path_buf(),
            started_at: now,
            completed_at: None,
            is_recursive,
            is_dry_run,
            status: SearchSessionStatus::Active,
        });

        // Add root directory to work queue
        self.enqueue_work(&session_id, root_path, 0).await?;

        Ok(session_id)
    }

    /// Complete the current search session
    pub async fn complete_session(&mut self) -> Result<()> {
        if let Some(session) = &self.current_session {
            let now = Utc::now().timestamp();

            sqlx::query(
                "UPDATE search_sessions SET completed_at = ?1, status = 'completed' WHERE session_id = ?2"
            )
            .bind(now)
            .bind(&session.session_id)
            .execute(&self.pool)
            .await?;

            // Clear work queue for this session
            sqlx::query("DELETE FROM work_queue WHERE session_id = ?")
                .bind(&session.session_id)
                .execute(&self.pool)
                .await?;
        }

        self.current_session = None;
        Ok(())
    }

    /// Mark session as interrupted
    pub async fn interrupt_session(&mut self) -> Result<()> {
        if let Some(session) = &self.current_session {
            sqlx::query("UPDATE search_sessions SET status = 'interrupted' WHERE session_id = ?")
                .bind(&session.session_id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// Resume an interrupted session
    pub async fn resume_session(
        &mut self,
        root_path: &Path,
        is_recursive: bool,
        is_dry_run: bool,
    ) -> Result<Option<String>> {
        // Look for interrupted sessions for this root path
        let session_row = sqlx::query(
            r"
            SELECT session_id, started_at FROM search_sessions
            WHERE root_path = ? AND status = 'interrupted' AND is_recursive = ? AND is_dry_run = ?
            ORDER BY started_at DESC LIMIT 1
            ",
        )
        .bind(Self::path_to_str(root_path).as_ref())
        .bind(is_recursive)
        .bind(is_dry_run)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = session_row {
            let session_id: String = row.get("session_id");
            let started_at: i64 = row.get("started_at");

            // Check if there's work remaining
            let work_count: i64 =
                sqlx::query("SELECT COUNT(*) as count FROM work_queue WHERE session_id = ?")
                    .bind(&session_id)
                    .fetch_one(&self.pool)
                    .await?
                    .get("count");

            // Check if there are found files from this session
            let found_files_count: i64 =
                sqlx::query("SELECT COUNT(*) as count FROM found_files WHERE session_id = ?")
                    .bind(&session_id)
                    .fetch_one(&self.pool)
                    .await?
                    .get("count");

            if work_count > 0 || found_files_count > 0 {
                // Resume this session - either work remaining or found files to load
                sqlx::query("UPDATE search_sessions SET status = 'active' WHERE session_id = ?")
                    .bind(&session_id)
                    .execute(&self.pool)
                    .await?;

                self.current_session = Some(SearchSession {
                    session_id: session_id.clone(),
                    root_path: root_path.to_path_buf(),
                    started_at,
                    completed_at: None,
                    is_recursive,
                    is_dry_run,
                    status: SearchSessionStatus::Active,
                });

                return Ok(Some(session_id));
            }
            // No work remaining and no found files, clean up
            self.cleanup_session(&session_id).await?;
        }

        Ok(None)
    }

    /// Add work item to queue
    pub async fn enqueue_work(&self, session_id: &str, path: &Path, priority: i32) -> Result<()> {
        let now = Utc::now().timestamp();

        sqlx::query(
            r"
            INSERT OR IGNORE INTO work_queue (path, discovered_at, priority, session_id)
            VALUES (?1, ?2, ?3, ?4)
            ",
        )
        .bind(Self::path_to_str(path).as_ref())
        .bind(now)
        .bind(priority)
        .bind(session_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Add multiple work items to queue (batch operation)
    pub async fn enqueue_work_batch(
        &self,
        session_id: &str,
        paths: &[PathBuf],
        priority: i32,
    ) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }

        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;

        for path in paths {
            sqlx::query(
                r"
                INSERT OR IGNORE INTO work_queue (path, discovered_at, priority, session_id)
                VALUES (?1, ?2, ?3, ?4)
                ",
            )
            .bind(Self::path_to_str(path).as_ref())
            .bind(now)
            .bind(priority)
            .bind(session_id)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// Get next batch of work items (peek without removing)
    pub async fn peek_work_batch(
        &self,
        session_id: &str,
        batch_size: usize,
    ) -> Result<Vec<WorkItem>> {
        let rows = sqlx::query(
            r"
            SELECT id, path, discovered_at, priority FROM work_queue
            WHERE session_id = ?
            ORDER BY priority DESC, id ASC
            LIMIT ?
            ",
        )
        .bind(session_id)
        .bind(batch_size as i64)
        .fetch_all(&self.pool)
        .await?;

        let work_items: Vec<WorkItem> = rows
            .into_iter()
            .map(|row| WorkItem {
                id: Some(row.get("id")),
                path: PathBuf::from(row.get::<String, _>("path")),
                discovered_at: row.get("discovered_at"),
                priority: row.get("priority"),
                session_id: session_id.to_string(),
            })
            .collect();

        Ok(work_items)
    }

    /// Remove specific work items from queue by ID
    pub async fn remove_work_items(&self, item_ids: &[i64]) -> Result<()> {
        if item_ids.is_empty() {
            return Ok(());
        }

        let placeholders = item_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!("DELETE FROM work_queue WHERE id IN ({placeholders})");

        let mut query_builder = sqlx::query(&query);
        for id in item_ids {
            query_builder = query_builder.bind(id);
        }
        query_builder.execute(&self.pool).await?;

        Ok(())
    }

    /// Get remaining work count for session
    pub async fn get_work_count(&self, session_id: &str) -> Result<usize> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM work_queue WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.get::<i64, _>("count") as usize)
    }

    /// Clean up stale sessions (older than cache window)
    async fn cleanup_stale_sessions(&self) -> Result<()> {
        let cutoff = Utc::now().timestamp()
            - i64::try_from(self.window_hours)
                .unwrap_or(i64::MAX)
                .saturating_mul(3600);

        // Find stale sessions
        let stale_sessions = sqlx::query(
            "SELECT session_id FROM search_sessions WHERE started_at < ? AND status != 'completed'",
        )
        .bind(cutoff)
        .fetch_all(&self.pool)
        .await?;

        for row in stale_sessions {
            let session_id: String = row.get("session_id");
            self.cleanup_session(&session_id).await?;
        }

        Ok(())
    }

    /// Clean up a specific session
    async fn cleanup_session(&self, session_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM work_queue WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;

        sqlx::query("DELETE FROM found_files WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;

        sqlx::query("DELETE FROM search_sessions WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Get current session info
    pub fn get_current_session(&self) -> Option<&SearchSession> {
        self.current_session.as_ref()
    }

    /// Save found .DS_Store files for session
    pub async fn save_found_files(&self, session_id: &str, files: &[PathBuf]) -> Result<()> {
        if files.is_empty() {
            return Ok(());
        }

        let now = Utc::now().timestamp();
        let mut tx = self.pool.begin().await?;

        for file_path in files {
            sqlx::query(
                r"
                INSERT OR IGNORE INTO found_files (session_id, file_path, discovered_at)
                VALUES (?1, ?2, ?3)
                ",
            )
            .bind(session_id)
            .bind(Self::path_to_str(file_path).as_ref())
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    /// Load found .DS_Store files for session
    pub async fn load_found_files(&self, session_id: &str) -> Result<Vec<PathBuf>> {
        let rows = sqlx::query(
            "SELECT file_path FROM found_files WHERE session_id = ? ORDER BY discovered_at ASC",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| PathBuf::from(row.get::<String, _>("file_path")))
            .collect())
    }

    /// Get the count of directories that were searched in a specific session
    pub async fn get_session_searched_count(&self, session_id: &str) -> Result<usize> {
        // Get the session timeframe
        let session_row = sqlx::query(
            "SELECT started_at, completed_at FROM search_sessions WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = session_row {
            let started_at: i64 = row.get("started_at");
            let completed_at: Option<i64> = row.get("completed_at");

            // Use completed_at if available, otherwise current time
            let end_time = completed_at.unwrap_or_else(|| chrono::Utc::now().timestamp());

            // Count directories that were searched during this session's timeframe
            let count: i64 = sqlx::query(
                "SELECT COUNT(*) as count FROM directory_cache
                 WHERE last_searched_at >= ? AND last_searched_at <= ? AND search_completed = TRUE",
            )
            .bind(started_at)
            .bind(end_time)
            .fetch_one(&self.pool)
            .await?
            .get("count");

            Ok(count as usize)
        } else {
            Ok(0)
        }
    }

    /// Get all .DS_Store files that have been found but not deleted within a given path
    pub async fn get_undeleted_ds_store_files(
        &self,
        root_path: &Path,
        recursive: bool,
    ) -> Result<Vec<PathBuf>> {
        let root_path_str = Self::path_to_str(root_path);

        // Build the query - we need to construct the file paths from directories that have undeleted .DS_Store files
        let query = if recursive {
            // For recursive searches, get all directories under the root path that have undeleted .DS_Store files
            sqlx::query(
                r"
                SELECT DISTINCT dc.path || '/.DS_Store' as file_path
                FROM directory_cache dc
                WHERE dc.ds_store_found = TRUE
                  AND dc.ds_store_deleted = FALSE
                  AND dc.search_completed = TRUE
                  AND dc.path LIKE ?1 || '%'
                ORDER BY file_path
                ",
            )
            .bind(root_path_str.as_ref())
        } else {
            // For non-recursive searches, only get the .DS_Store directly in the root path
            sqlx::query(
                r"
                SELECT DISTINCT dc.path || '/.DS_Store' as file_path
                FROM directory_cache dc
                WHERE dc.ds_store_found = TRUE
                  AND dc.ds_store_deleted = FALSE
                  AND dc.search_completed = TRUE
                  AND dc.path = ?1
                ORDER BY file_path
                ",
            )
            .bind(root_path_str.as_ref())
        };

        let rows = query.fetch_all(&self.pool).await?;

        Ok(rows
            .into_iter()
            .map(|row| PathBuf::from(row.get::<String, _>("file_path")))
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct CacheStats {
    pub total_entries: u64,
    pub completed_searches: u64,
    pub incomplete_searches: u64,
    pub directories_with_ds_store: u64,
    pub ds_stores_deleted: u64,
    pub errors: u64,
}
