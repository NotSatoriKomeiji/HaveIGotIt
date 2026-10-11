use crate::{files::Record, names::Name};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

pub struct Db {
    pub conn: Connection,
}

#[derive(Debug, Clone)]
pub struct Folder {
    pub id: i64,
    pub path: PathBuf,
}

pub struct IndexedFile {
    pub key: String,
    pub folder_id: i64,
    pub record: Record,
}

pub fn key(path: &Path) -> String {
    let value = path.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        value.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value
    }
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(3))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA cache_size=-1024;
            CREATE TABLE IF NOT EXISTS folders(id INTEGER PRIMARY KEY, path TEXT NOT NULL, path_key TEXT NOT NULL UNIQUE);
            CREATE TABLE IF NOT EXISTS files(
              path_key TEXT PRIMARY KEY, path TEXT NOT NULL, folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
              canonical TEXT NOT NULL, identity TEXT NOT NULL, extension TEXT NOT NULL, name_version TEXT,
              size INTEGER NOT NULL, modified_ns TEXT NOT NULL, age_time INTEGER NOT NULL, pe_version TEXT, sample TEXT,
              scan_generation INTEGER NOT NULL DEFAULT 0);
            CREATE INDEX IF NOT EXISTS files_name_size ON files(canonical,size);
            CREATE INDEX IF NOT EXISTS files_identity ON files(identity);
            CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            PRAGMA user_version=1;")?;
        Ok(Self { conn })
    }
    pub fn folders(&self) -> rusqlite::Result<Vec<Folder>> {
        self.conn
            .prepare("SELECT id,path FROM folders ORDER BY id")?
            .query_map([], |row| {
                Ok(Folder {
                    id: row.get(0)?,
                    path: PathBuf::from(row.get::<_, String>(1)?),
                })
            })?
            .collect()
    }
    pub fn add_folder(&self, path: &Path) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO folders(path,path_key) VALUES(?1,?2)",
            params![path.to_string_lossy(), key(path)],
        )?;
        Ok(self.conn.last_insert_rowid())
    }
    pub fn remove_folder(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM folders WHERE id=?1", [id])?;
        Ok(())
    }
    pub fn get(&self, path: &Path) -> rusqlite::Result<Option<Record>> {
        self.conn.query_row("SELECT path,canonical,identity,extension,name_version,size,modified_ns,age_time,pe_version,sample FROM files WHERE path_key=?1",[key(path)],record).optional()
    }
    pub fn put(&self, folder_id: i64, value: &Record, generation: i64) -> rusqlite::Result<()> {
        self.conn.execute("INSERT INTO files(path_key,path,folder_id,canonical,identity,extension,name_version,size,modified_ns,age_time,pe_version,sample,scan_generation)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
            ON CONFLICT(path_key) DO UPDATE SET path=excluded.path,folder_id=excluded.folder_id,canonical=excluded.canonical,
              identity=excluded.identity,extension=excluded.extension,name_version=excluded.name_version,size=excluded.size,
              modified_ns=excluded.modified_ns,age_time=excluded.age_time,pe_version=excluded.pe_version,sample=excluded.sample,scan_generation=excluded.scan_generation",
            params![key(&value.path),value.path.to_string_lossy(),folder_id,value.name.canonical,value.name.identity,
                value.name.extension,value.name.version,value.size as i64,value.modified_ns,value.age_time,value.pe_version,value.sample,generation])?;
        Ok(())
    }
    pub fn candidates(&self, value: &Record) -> rusqlite::Result<Vec<Record>> {
        // No full-table extension scan. Named matches use size-indexed bounds;
        // version-age candidates use the preserved architecture/channel identity.
        let lower = (u128::from(value.size) * 9 / 10) as i64;
        let upper = (u128::from(value.size) * 10 / 9).min(i64::MAX as u128) as i64;
        self.conn.prepare("SELECT path,canonical,identity,extension,name_version,size,modified_ns,age_time,pe_version,sample
            FROM files WHERE ((canonical=?1 AND size BETWEEN ?2 AND ?3) OR identity=?4) AND path_key<>?5
            ORDER BY CASE WHEN canonical=?1 THEN 0 ELSE 1 END,age_time,path_key LIMIT 128")?
            .query_map(params![value.name.canonical,lower,upper,value.name.identity,key(&value.path)],record)?.collect()
    }
    pub fn named_candidates(
        &self,
        name: &Name,
        final_path: &Path,
    ) -> rusqlite::Result<Vec<Record>> {
        // The existing (canonical,size) index has canonical as its left prefix.
        // Exact names preserve known filename versions, architecture and channel.
        self.conn.prepare("SELECT path,canonical,identity,extension,name_version,size,modified_ns,age_time,pe_version,sample
            FROM files WHERE canonical=?1 AND path_key<>?2 LIMIT 128")?
            .query_map(params![name.canonical,key(final_path)],record)?.collect()
    }
    pub fn delete_path(&self, path: &Path) -> rusqlite::Result<usize> {
        // An indexed component-prefix range, not substr/LIKE or a table scan.
        // The separator is ASCII ('/' or '\\'); its successor is a safe upper
        // bound in SQLite's binary collation, including Unicode descendants.
        let base = key(path);
        let prefix = format!(
            "{}{}",
            base.trim_end_matches(std::path::MAIN_SEPARATOR),
            std::path::MAIN_SEPARATOR
        );
        let upper = format!(
            "{}{}",
            base.trim_end_matches(std::path::MAIN_SEPARATOR),
            char::from_u32(std::path::MAIN_SEPARATOR as u32 + 1).unwrap()
        );
        self.conn.execute(
            "DELETE FROM files WHERE path_key=?1 OR (path_key>=?2 AND path_key<?3)",
            params![base, prefix, upper],
        )
    }
    pub fn clean_scan(&self, id: i64, generation: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM files WHERE folder_id=?1 AND scan_generation<>?2",
            params![id, generation],
        )?;
        Ok(())
    }
    pub fn count(&self) -> rusqlite::Result<u64> {
        self.conn
            .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
    }
    pub fn last_key(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT max(path_key) FROM files", [], |r| r.get(0))
    }
    pub fn verification_page(
        &self,
        after: &str,
        until: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<IndexedFile>> {
        // Keyset paging stays bounded in memory and avoids growing OFFSET scans.
        self.conn.prepare("SELECT path,canonical,identity,extension,name_version,size,modified_ns,age_time,pe_version,sample,path_key,folder_id
            FROM files WHERE path_key>?1 AND path_key<=?2 ORDER BY path_key LIMIT ?3")?
            .query_map(params![after, until, limit as i64], |row| Ok(IndexedFile {
                record: record(row)?, key: row.get(10)?, folder_id: row.get(11)?,
            }))?.collect()
    }
    pub fn remove_record(&self, key: &str) -> rusqlite::Result<usize> {
        self.conn
            .execute("DELETE FROM files WHERE path_key=?1", [key])
    }
    pub fn invalidate_evidence(&self, key: &str) -> rusqlite::Result<usize> {
        // Keep the completed fingerprint: advancing it during background checking
        // could swallow a queued download event for this same destination.
        self.conn.execute(
            "UPDATE files SET sample=NULL,pe_version=NULL WHERE path_key=?1
            AND (sample IS NOT NULL OR pe_version IS NOT NULL)",
            [key],
        )
    }
    pub fn setting(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()
    }
    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[key,value])?;
        Ok(())
    }
}

fn record(row: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record {
        path: PathBuf::from(row.get::<_, String>(0)?),
        name: Name {
            canonical: row.get(1)?,
            identity: row.get(2)?,
            extension: row.get(3)?,
            version: row.get(4)?,
        },
        size: row.get::<_, i64>(5)? as u64,
        modified_ns: row.get(6)?,
        age_time: row.get(7)?,
        pe_version: row.get(8)?,
        sample: row.get(9)?,
    })
}
