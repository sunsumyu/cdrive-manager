use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::ScanStats;

#[derive(Debug, Clone)]
pub struct ScanCacheEntry {
    pub root_key: String,
    pub root_display: String,
    pub schema_version: u32,
    pub app_version: Option<String>,
    pub saved_at_unix_secs: u64,
    pub total_size: u64,
    pub file_count: u64,
    pub dir_count: u64,
    pub error_count: u64,
}

pub fn default_cache_db_path() -> anyhow::Result<PathBuf> {
    if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data)
            .join("cdrive-manager")
            .join("scan-cache.sqlite3"));
    }

    Ok(env::current_dir()?.join(".cdrive-manager-cache.sqlite3"))
}

pub fn save_latest_scan(stats: &ScanStats) -> anyhow::Result<PathBuf> {
    let db_path = default_cache_db_path()?;
    save_latest_scan_to_path(&db_path, stats)?;
    Ok(db_path)
}

pub fn load_latest_scan(root: &Path) -> anyhow::Result<Option<ScanStats>> {
    let db_path = default_cache_db_path()?;
    if !db_path.exists() {
        return Ok(None);
    }
    load_latest_scan_from_path(&db_path, root)
}

pub fn save_latest_scan_to_path(db_path: &Path, stats: &ScanStats) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let mut stats = stats.clone();
    stats.prepare_for_save();
    let stats_json = serde_json::to_vec(&stats)?;
    let root_key = root_key(&stats.root);
    let saved_at = stats.saved_at_unix_secs.unwrap_or_default();

    connection.execute(
        "INSERT INTO scan_cache (
            root_key,
            root_display,
            schema_version,
            app_version,
            saved_at_unix_secs,
            total_size,
            file_count,
            dir_count,
            error_count,
            stats_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        ON CONFLICT(root_key) DO UPDATE SET
            root_display = excluded.root_display,
            schema_version = excluded.schema_version,
            app_version = excluded.app_version,
            saved_at_unix_secs = excluded.saved_at_unix_secs,
            total_size = excluded.total_size,
            file_count = excluded.file_count,
            dir_count = excluded.dir_count,
            error_count = excluded.error_count,
            stats_json = excluded.stats_json",
        params![
            root_key,
            stats.root.display().to_string(),
            stats.schema_version as i64,
            stats.app_version.as_deref(),
            saved_at as i64,
            stats.total_size as i64,
            stats.file_count as i64,
            stats.dir_count as i64,
            stats.error_count as i64,
            stats_json,
        ],
    )?;

    Ok(())
}

pub fn load_latest_scan_from_path(
    db_path: &Path,
    root: &Path,
) -> anyhow::Result<Option<ScanStats>> {
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let stats_json: Option<Vec<u8>> = connection
        .query_row(
            "SELECT stats_json FROM scan_cache WHERE root_key = ?1",
            params![root_key(root)],
            |row| row.get(0),
        )
        .optional()?;

    stats_json
        .map(|stats_json| {
            let mut stats: ScanStats = serde_json::from_slice(&stats_json)
                .context("SQLite 缓存中的扫描结果 JSON 无法解析")?;
            stats.normalize_cache_metadata_after_load();
            stats.rebuild_indexes();
            Ok(stats)
        })
        .transpose()
}

fn initialize_schema(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS scan_cache (
            root_key TEXT PRIMARY KEY,
            root_display TEXT NOT NULL,
            schema_version INTEGER NOT NULL,
            app_version TEXT,
            saved_at_unix_secs INTEGER NOT NULL,
            total_size INTEGER NOT NULL,
            file_count INTEGER NOT NULL,
            dir_count INTEGER NOT NULL,
            error_count INTEGER NOT NULL,
            stats_json BLOB NOT NULL
        );",
    )
}

fn root_key(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical
        .display()
        .to_string()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

pub fn list_scan_cache_entries(db_path: &Path) -> anyhow::Result<Vec<ScanCacheEntry>> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let mut stmt = connection.prepare(
        "SELECT root_key, root_display, schema_version, app_version, saved_at_unix_secs, total_size, file_count, dir_count, error_count
         FROM scan_cache ORDER BY saved_at_unix_secs DESC",
    )?;

    let entries = stmt
        .query_map([], |row| {
            Ok(ScanCacheEntry {
                root_key: row.get(0)?,
                root_display: row.get(1)?,
                schema_version: row.get::<_, i64>(2)? as u32,
                app_version: row.get(3)?,
                saved_at_unix_secs: row.get::<_, i64>(4)? as u64,
                total_size: row.get::<_, i64>(5)? as u64,
                file_count: row.get::<_, i64>(6)? as u64,
                dir_count: row.get::<_, i64>(7)? as u64,
                error_count: row.get::<_, i64>(8)? as u64,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(entries)
}

pub fn load_scan_cache_by_root_key(
    db_path: &Path,
    root_key: &str,
) -> anyhow::Result<Option<ScanStats>> {
    if !db_path.exists() {
        return Ok(None);
    }
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let stats_json: Option<Vec<u8>> = connection
        .query_row(
            "SELECT stats_json FROM scan_cache WHERE root_key = ?1",
            params![root_key],
            |row| row.get(0),
        )
        .optional()?;

    stats_json
        .map(|stats_json| {
            let mut stats: ScanStats = serde_json::from_slice(&stats_json)
                .context("SQLite 缓存中的扫描结果 JSON 无法解析")?;
            stats.normalize_cache_metadata_after_load();
            stats.rebuild_indexes();
            Ok(stats)
        })
        .transpose()
}

pub fn delete_scan_cache_by_root_key(db_path: &Path, root_key: &str) -> anyhow::Result<bool> {
    if !db_path.exists() {
        return Ok(false);
    }
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let rows_deleted = connection.execute(
        "DELETE FROM scan_cache WHERE root_key = ?1",
        params![root_key],
    )?;

    Ok(rows_deleted > 0)
}

pub fn get_cache_db_size(db_path: &Path) -> anyhow::Result<Option<u64>> {
    if !db_path.exists() {
        return Ok(None);
    }
    let metadata = fs::metadata(db_path)?;
    Ok(Some(metadata.len()))
}

pub fn format_saved_at_time(saved_at_unix_secs: u64) -> String {
    if saved_at_unix_secs == 0 {
        return "未知时间".to_owned();
    }
    let datetime = chrono_timestamp(saved_at_unix_secs);
    datetime
}

fn chrono_timestamp(unix_secs: u64) -> String {
    // Simple formatting without chrono dependency
    let days = unix_secs / 86400;
    let hours = (unix_secs % 86400) / 3600;
    let minutes = (unix_secs % 3600) / 60;
    let secs = unix_secs % 60;

    // Approximate date calculation (not precise but sufficient for display)
    let years_since_1970 = days / 365;
    let remaining_days = days % 365;
    let months_approx = remaining_days / 30;
    let day_approx = remaining_days % 30;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        1970 + years_since_1970,
        months_approx + 1,
        day_approx + 1,
        hours,
        minutes,
        secs
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FileRecord;

    #[test]
    fn root_key_normalizes_case_and_slashes() {
        assert_eq!(
            root_key(Path::new("C:/Users/Alice/")),
            root_key(Path::new("c:\\users\\alice"))
        );
    }

    #[test]
    fn sqlite_cache_round_trips_scan_stats() {
        let db_path = env::temp_dir().join(format!(
            "cdrive-manager-test-{}.sqlite3",
            std::process::id()
        ));
        let _ = fs::remove_file(&db_path);
        let root = env::temp_dir().join(format!("cdrive-manager-root-{}", std::process::id()));
        let mut stats = ScanStats::default();
        stats.root = root.clone();
        stats.total_size = 10;
        stats.file_count = 1;
        stats.all_files.push(FileRecord {
            path: root.join("a.bin"),
            size: 10,
            modified: None,
            extension: ".bin".to_owned(),
        });

        save_latest_scan_to_path(&db_path, &stats).unwrap();
        let loaded = load_latest_scan_from_path(&db_path, &root)
            .unwrap()
            .unwrap();

        assert_eq!(loaded.root, root);
        assert_eq!(loaded.total_size, 10);
        assert_eq!(loaded.all_files.len(), 1);
        assert!(loaded.saved_at_unix_secs.is_some());

        let _ = fs::remove_file(&db_path);
    }
}
