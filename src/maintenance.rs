//! Bounded metadata-only validation of indexed paths. Never modify user files.
use crate::{
    db::{Db, Folder},
    files,
};
use std::{
    fs,
    io::ErrorKind,
    path::Path,
    time::{Duration, Instant},
};

pub const DELETE_QUIET: Duration = Duration::from_secs(1);
pub const BATCH_PAUSE: Duration = Duration::from_secs(1);
pub const BATCH_SIZE: usize = 64;
const BUDGET: Duration = Duration::from_millis(8);

#[derive(Default)]
pub struct Verifier {
    cursor: String,
    upper: Option<String>,
}

/// No recurring timer. Each quiet-separated deletion burst starts one sweep.
/// More deletions pause an in-flight sweep, then reset its cursor once the new
/// burst settles, so rows behind that cursor cannot escape verification.
#[derive(Default)]
pub struct DeletionChecks {
    quiet_until: Option<Instant>,
    next_batch: Option<Instant>,
    pub verifier: Verifier,
}
impl DeletionChecks {
    pub fn note_delete(&mut self, now: Instant) {
        self.quiet_until = Some(now + DELETE_QUIET);
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.quiet_until.or(self.next_batch)
    }
    pub fn begin_if_ready(&mut self, now: Instant) -> bool {
        if self.quiet_until.is_some_and(|due| due <= now) {
            self.quiet_until = None;
            self.verifier = Verifier::default();
            self.next_batch = Some(now);
            true
        } else {
            false
        }
    }
    pub fn batch_ready(&self, now: Instant) -> bool {
        self.quiet_until.is_none() && self.next_batch.is_some_and(|due| due <= now)
    }
    pub fn after_batch(&mut self, complete: bool, now: Instant) {
        self.next_batch = if complete {
            None
        } else {
            Some(now + BATCH_PAUSE)
        };
    }
}
#[derive(Default, Debug)]
pub struct Progress {
    pub checked: usize,
    pub removed: usize,
    pub invalidated: usize,
    pub complete: bool,
}
impl Verifier {
    /// One page, then release its Vec. The time budget is soft: a filesystem
    /// metadata call itself cannot be interrupted. No hashes or EXE resources.
    pub fn step(
        &mut self,
        db: &Db,
        folders: &[Folder],
        skip: impl Fn(&Path) -> bool,
    ) -> rusqlite::Result<Progress> {
        if self.upper.is_none() {
            self.upper = db.last_key()?;
        }
        let Some(upper) = &self.upper else {
            self.finish(db)?;
            return Ok(Progress {
                complete: true,
                ..Progress::default()
            });
        };
        let page = db.verification_page(&self.cursor, upper, BATCH_SIZE)?;
        if page.is_empty() {
            self.finish(db)?;
            return Ok(Progress {
                complete: true,
                ..Progress::default()
            });
        }
        let page_len = page.len();
        let start = Instant::now();
        let mut progress = Progress::default();
        // At most one root availability check per root in this 64-record page.
        let mut roots = Vec::<(i64, bool)>::new();
        let transaction = db.conn.unchecked_transaction()?;
        let mut cursor = self.cursor.clone();
        for indexed in page {
            cursor = indexed.key.clone();
            progress.checked += 1;
            let path = &indexed.record.path;
            if !skip(path) {
                if let Some(folder) = folders.iter().find(|f| f.id == indexed.folder_id) {
                    if !files::within_root_without_links(path, &folder.path) {
                        // Discard invalid keys without accessing an unauthorized path.
                        progress.removed += db.remove_record(&indexed.key)?;
                        continue;
                    }
                    let available = if let Some((_, available)) =
                        roots.iter().find(|(id, _)| *id == folder.id)
                    {
                        *available
                    } else {
                        let available = !files::is_link(&folder.path)
                            && fs::metadata(&folder.path).is_ok_and(|m| m.is_dir());
                        roots.push((folder.id, available));
                        available
                    };
                    if available {
                        match fs::symlink_metadata(path) {
                            Err(e) if e.kind() == ErrorKind::NotFound => {
                                // An unplugged/unavailable root must not look like
                                // 64 deleted files. Recheck before removing a row.
                                if fs::metadata(&folder.path).is_ok_and(|m| m.is_dir()) {
                                    progress.removed += db.remove_record(&indexed.key)?;
                                }
                            }
                            Ok(meta) if !meta.is_file() || files::is_link(path) => {
                                progress.removed += db.remove_record(&indexed.key)?;
                            }
                            Ok(_) if files::permitted(path, &folder.path) => {
                                match files::inspect_metadata(path) {
                                    Ok(Some(current))
                                        if files::fingerprint(&current)
                                            != files::fingerprint(&indexed.record) =>
                                    {
                                        progress.invalidated +=
                                            db.invalidate_evidence(&indexed.key)?;
                                    }
                                    Ok(None) => {
                                        progress.removed += db.remove_record(&indexed.key)?;
                                    }
                                    _ => {} // Unreadable paths retain their records.
                                }
                            }
                            _ => {} // Permission errors and inaccessible parents are preserved.
                        }
                    }
                }
            }
            if start.elapsed() >= BUDGET {
                break;
            }
        }
        transaction.commit()?;
        self.cursor = cursor;
        if progress.checked == page_len && (page_len < BATCH_SIZE || self.cursor == *upper) {
            self.finish(db)?;
            progress.complete = true;
        }
        Ok(progress)
    }
    fn finish(&mut self, db: &Db) -> rusqlite::Result<()> {
        // Return idle SQLite page-cache allocations. Freed DB pages are reused;
        // no costly VACUUM, full-index copy or forced working-set trimming.
        db.conn.execute_batch("PRAGMA shrink_memory")?;
        self.cursor.clear();
        self.upper = None;
        Ok(())
    }
}
