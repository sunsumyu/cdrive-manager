//! FRN(File Reference Number)→路径 映射的 SQLite 存储。
//!
//! USN Journal 通过 FRN 标识文件, 但搜索索引需要完整路径。此模块维护
//! `(root_key, frn) → path` 的 KV 映射, 供 USN 增量更新时解析路径。

use rusqlite::{Connection, OptionalExtension, params};
use crate::search_index::schema::frn_db_path;

/// 打开 FRN 映射数据库并初始化表结构。
pub fn open_frn_db() -> rusqlite::Result<Connection> {
    let path = frn_db_path().map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(e.into())
    })?;
    let conn = Connection::open(&path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS frn_path_map (
             root_key TEXT NOT NULL,
             frn TEXT NOT NULL,
             path TEXT NOT NULL,
             parent_frn TEXT,
             is_directory INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (root_key, frn)
         );
         CREATE INDEX IF NOT EXISTS idx_frn_root ON frn_path_map(root_key, frn);
         CREATE INDEX IF NOT EXISTS idx_frn_parent ON frn_path_map(root_key, parent_frn);",
    )?;
    Ok(conn)
}

/// 插入或更新 FRN→路径 映射。
pub fn upsert_frn_path(
    conn: &Connection,
    root_key: &str,
    frn: &str,
    path: &str,
    parent_frn: Option<&str>,
    is_directory: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO frn_path_map (root_key, frn, path, parent_frn, is_directory)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(root_key, frn) DO UPDATE SET
            path = excluded.path,
            parent_frn = excluded.parent_frn,
            is_directory = excluded.is_directory",
        params![root_key, frn, path, parent_frn, is_directory as i32],
    )?;
    Ok(())
}

/// 通过 FRN 查询已存储的路径。
pub fn lookup_frn_path(
    conn: &Connection,
    root_key: &str,
    frn: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT path FROM frn_path_map WHERE root_key = ?1 AND frn = ?2 LIMIT 1",
        params![root_key, frn],
        |row| row.get::<_, String>(0),
    )
    .optional()
}

/// 通过父 FRN 与文件名解析路径(父路径 + 文件名)。
pub fn resolve_path_from_frn(
    conn: &Connection,
    root_key: &str,
    parent_frn: &str,
    file_name: &str,
) -> rusqlite::Result<Option<String>> {
    let parent_path: Option<String> = conn
        .query_row(
            "SELECT path FROM frn_path_map WHERE root_key = ?1 AND frn = ?2 LIMIT 1",
            params![root_key, parent_frn],
            |row| row.get(0),
        )
        .optional()?;
    Ok(parent_path.map(|p| format!("{}\\{}", p, file_name)))
}

/// 删除指定 FRN 的映射。
pub fn delete_frn_path(
    conn: &Connection,
    root_key: &str,
    frn: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM frn_path_map WHERE root_key = ?1 AND frn = ?2",
        params![root_key, frn],
    )?;
    Ok(())
}

/// 清除指定 root_key 的所有 FRN 映射(用于全量重建)。
pub fn clear_frn_for_root(conn: &Connection, root_key: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM frn_path_map WHERE root_key = ?1",
        params![root_key],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_temp_db() -> Connection {
        let tmp = std::env::temp_dir().join(format!(
            "cdrive-frn-test-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp);
        // 直接创建 Connection, 绕过 LOCALAPPDATA 依赖, 避免并行测试冲突
        let conn = Connection::open(&tmp).expect("应能打开临时 DB");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS frn_path_map (
                 root_key TEXT NOT NULL,
                 frn TEXT NOT NULL,
                 path TEXT NOT NULL,
                 parent_frn TEXT,
                 is_directory INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (root_key, frn)
             );
             CREATE INDEX IF NOT EXISTS idx_frn_root ON frn_path_map(root_key, frn);
             CREATE INDEX IF NOT EXISTS idx_frn_parent ON frn_path_map(root_key, parent_frn);",
        ).expect("应能初始化表结构");
        conn
    }

    #[test]
    fn upsert_and_lookup_round_trip() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\file.txt", Some("50"), false).unwrap();
        let path = lookup_frn_path(&conn, "c:/", "100").unwrap();
        assert_eq!(path.as_deref(), Some("C:\\file.txt"));
    }

    #[test]
    fn upsert_overwrites_existing() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\old.txt", None, false).unwrap();
        upsert_frn_path(&conn, "c:/", "100", "C:\\new.txt", None, false).unwrap();
        let path = lookup_frn_path(&conn, "c:/", "100").unwrap();
        assert_eq!(path.as_deref(), Some("C:\\new.txt"));
    }

    #[test]
    fn resolve_path_from_parent_frn() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "50", "C:\\Users", Some("1"), true).unwrap();
        let resolved = resolve_path_from_frn(&conn, "c:/", "50", "file.txt").unwrap();
        assert_eq!(resolved.as_deref(), Some("C:\\Users\\file.txt"));
    }

    #[test]
    fn delete_removes_mapping() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\file.txt", None, false).unwrap();
        delete_frn_path(&conn, "c:/", "100").unwrap();
        let path = lookup_frn_path(&conn, "c:/", "100").unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn clear_for_root_removes_all() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\a.txt", None, false).unwrap();
        upsert_frn_path(&conn, "c:/", "101", "C:\\b.txt", None, false).unwrap();
        upsert_frn_path(&conn, "d:/", "200", "D:\\c.txt", None, false).unwrap();
        clear_frn_for_root(&conn, "c:/").unwrap();
        assert!(lookup_frn_path(&conn, "c:/", "100").unwrap().is_none());
        assert!(lookup_frn_path(&conn, "c:/", "101").unwrap().is_none());
        assert!(lookup_frn_path(&conn, "d:/", "200").unwrap().is_some());
    }
}
