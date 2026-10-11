use crate::{
    db::{Db, Folder},
    downloads,
    files::{self, Record},
    maintenance,
    matching::{self, Alert, Reason},
    names,
};
use notify::{
    event::{ModifyKind, RenameMode},
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use walkdir::WalkDir;

const QUIET: Duration = Duration::from_millis(150);
const MAX_PENDING: usize = 4096;

pub enum Command {
    Add(PathBuf),
    Remove(i64),
    Rescan,
    Language(String),
    Stop,
}
pub enum Input {
    Command(Command),
    Fs(notify::Result<Event>),
}
#[derive(Debug, Clone)]
pub struct Status {
    pub folders: Vec<Folder>,
    pub files: u64,
    pub language: Option<String>,
    pub message: Option<String>,
}
pub enum Output {
    Status(Status),
    Alert(Box<Alert>),
    DownloadMoved {
        id: u64,
        path: PathBuf,
    },
    /// Update an existing hint only. Never resurrect one that the user ignored.
    DownloadFinished {
        id: u64,
        alert: Option<Box<Alert>>,
    },
    Error(String),
    Stopped,
}

struct Pending {
    due: Instant,
    first: Instant,
    stable: Option<(u64, String)>,
    attempts: u32,
    completed_rename: bool,
    announced: Option<u64>,
}

struct Download {
    id: u64,
    temporary: PathBuf,
    announced: bool,
}

#[derive(Default)]
struct Downloads {
    active: HashMap<PathBuf, Download>,
    next_id: u64,
}
impl Downloads {
    fn begin(&mut self, path: PathBuf, db: &Db, emit: &impl Fn(Output), overflow: &AtomicBool) {
        let Some(target) = downloads::target(&path) else {
            return;
        };
        if self.active.contains_key(&target) || !path.is_file() {
            return;
        }
        if self.active.len() >= MAX_PENDING {
            // No per-download walk over all active downloads. Reclaim abandoned
            // entries only under capacity pressure, without an idle timer.
            self.active
                .retain(|target, d| d.temporary.exists() || target.exists());
            if self.active.len() >= MAX_PENDING {
                overflow.store(true, Ordering::Release);
                return;
            }
        }
        self.next_id += 1;
        let id = self.next_id;
        let announced = match downloads::preview(db, &path, &target) {
            Ok(Some(mut alert)) => {
                alert.download_id = Some(id);
                emit(Output::Alert(Box::new(alert)));
                true
            }
            Ok(None) => false,
            Err(e) => {
                emit(Output::Error(e));
                false
            }
        };
        self.active.insert(
            target,
            Download {
                id,
                temporary: path,
                announced,
            },
        );
    }
    fn finish(&mut self, path: &std::path::Path, emit: &impl Fn(Output)) -> Option<u64> {
        let download = self.active.remove(path)?;
        if download.announced {
            emit(Output::DownloadMoved {
                id: download.id,
                path: path.to_owned(),
            });
            Some(download.id)
        } else {
            None
        }
    }
    fn remove(&mut self, path: &std::path::Path, emit: &impl Fn(Output)) {
        self.active.retain(|target, download| {
            let removed = target.starts_with(path) || download.temporary.starts_with(path);
            if removed && download.announced {
                emit(Output::DownloadFinished {
                    id: download.id,
                    alert: None,
                });
            }
            !removed
        });
    }
}

/// A bounded command/event queue; the worker never waits on the UI's output.
pub fn start(database: PathBuf, emit: impl Fn(Output) + Send + 'static) -> SyncSender<Input> {
    let (tx, rx) = mpsc::sync_channel(1024);
    let out = tx.clone();
    thread::spawn(move || {
        let db = match Db::open(&database) {
            Ok(db) => db,
            Err(e) => {
                emit(Output::Error(e.to_string()));
                return;
            }
        };
        let overflow = Arc::new(AtomicBool::new(false));
        let callback_overflow = overflow.clone();
        let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |event| {
            if tx.try_send(Input::Fs(event)).is_err() {
                callback_overflow.store(true, Ordering::Release);
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                emit(Output::Error(e.to_string()));
                return;
            }
        };
        let mut folders = db.folders().unwrap_or_default();
        let mut pending = HashMap::<PathBuf, Pending>::new();
        let mut downloads = Downloads::default();
        let mut generation: i64 = db
            .conn
            .query_row(
                "SELECT coalesce(max(scan_generation),0) FROM files",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        for folder in &folders {
            if let Err(e) = watcher.watch(&folder.path, RecursiveMode::Recursive) {
                emit(Output::Error(format!("{}: {e}", folder.path.display())));
            }
            generation += 1;
            scan(&db, folder, generation, &emit);
        }
        status(&db, None, &emit);
        let mut deletion_checks = maintenance::DeletionChecks::default();
        loop {
            let verification = deletion_checks.deadline();
            let input =
                if let Some(next) = pending.values().map(|p| p.due).chain(verification).min() {
                    match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                        Ok(input) => Some(input),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                } else {
                    match rx.recv() {
                        Ok(input) => Some(input),
                        Err(_) => break,
                    }
                };
            if let Some(input) = input {
                match input {
                    Input::Command(Command::Stop) => break,
                    Input::Command(Command::Add(path)) => {
                        let result = (|| -> Result<(), String> {
                            let path = path.canonicalize().map_err(|e| e.to_string())?;
                            if !path.is_dir() {
                                return Err("Not a folder / 不是文件夹".into());
                            }
                            if path.to_str().is_none() {
                                return Err("Unsupported path encoding / 路径编码不受支持".into());
                            }
                            if !files::local_root(&path) {
                                return Err("Local folders only / 仅支持本地文件夹".into());
                            }
                            if folders
                                .iter()
                                .any(|f| path.starts_with(&f.path) || f.path.starts_with(&path))
                            {
                                return Err(
                                    "Folder overlaps an authorized folder / 文件夹与已授权目录重叠"
                                        .into(),
                                );
                            }
                            watcher
                                .watch(&path, RecursiveMode::Recursive)
                                .map_err(|e| e.to_string())?;
                            let id = match db.add_folder(&path) {
                                Ok(id) => id,
                                Err(e) => {
                                    let _ = watcher.unwatch(&path);
                                    return Err(e.to_string());
                                }
                            };
                            let folder = Folder { id, path };
                            generation += 1;
                            scan(&db, &folder, generation, &emit);
                            folders.push(folder);
                            Ok(())
                        })();
                        if let Err(e) = result {
                            emit(Output::Error(e));
                        }
                        status(&db, None, &emit);
                    }
                    Input::Command(Command::Remove(id)) => {
                        if let Some(folder) = folders.iter().find(|f| f.id == id) {
                            if let Err(e) = db.remove_folder(id) {
                                emit(Output::Error(e.to_string()));
                            } else {
                                let _ = watcher.unwatch(&folder.path);
                                pending.retain(|path, _| !path.starts_with(&folder.path));
                                downloads.remove(&folder.path, &emit);
                                folders.retain(|f| f.id != id);
                                pending.shrink_to_fit();
                                downloads.active.shrink_to_fit();
                                if folders.is_empty() {
                                    deletion_checks = maintenance::DeletionChecks::default();
                                }
                            }
                        }
                        status(&db, None, &emit);
                    }
                    Input::Command(Command::Language(language)) => {
                        if let Err(e) = db.set_setting("language", &language) {
                            emit(Output::Error(e.to_string()));
                        }
                        status(&db, None, &emit);
                    }
                    Input::Command(Command::Rescan) => {
                        for folder in &folders {
                            // Also restores watches after a temporarily disconnected volume.
                            let _ = watcher.unwatch(&folder.path);
                            if let Err(e) = watcher.watch(&folder.path, RecursiveMode::Recursive) {
                                emit(Output::Error(e.to_string()));
                            }
                            generation += 1;
                            scan(&db, folder, generation, &emit);
                        }
                        deletion_checks = maintenance::DeletionChecks::default();
                        status(&db, None, &emit);
                    }
                    Input::Fs(Err(e)) => {
                        emit(Output::Error(e.to_string()));
                        overflow.store(true, Ordering::Release);
                    }
                    Input::Fs(Ok(event)) => {
                        if event.need_rescan() {
                            overflow.store(true, Ordering::Release);
                        }
                        match event.kind {
                            EventKind::Remove(_) => {
                                for path in event.paths {
                                    if folders.iter().any(|f| path.starts_with(&f.path)) {
                                        match db.delete_path(&path) {
                                            Ok(removed) if removed > 0 => {
                                                deletion_checks.note_delete(Instant::now());
                                            }
                                            Err(e) => emit(Output::Error(e.to_string())),
                                            _ => {} // Unindexed temporary files need no sweep.
                                        }
                                        pending.retain(|p, _| !p.starts_with(&path));
                                        downloads.remove(&path, &emit);
                                    }
                                }
                            }
                            EventKind::Modify(ModifyKind::Name(RenameMode::Both))
                                if event.paths.len() == 2 =>
                            {
                                let (old, new) = (&event.paths[0], &event.paths[1]);
                                let was_indexed =
                                    db.get(old).ok().flatten().is_some() || new.is_dir();
                                if let Err(e) = db.delete_path(old) {
                                    emit(Output::Error(e.to_string()));
                                }
                                pending.remove(old);
                                if was_indexed {
                                    // A known file moved, not a new download. Baseline its destination.
                                    if let Some(folder) =
                                        folders.iter().find(|f| new.starts_with(&f.path))
                                    {
                                        if new.is_dir() {
                                            baseline_subtree(&db, folder, new, &emit);
                                        } else if files::permitted(new, &folder.path) {
                                            if let Ok(Some(record)) = files::inspect(new) {
                                                let _ = db.put(folder.id, &record, 0);
                                            }
                                        }
                                    }
                                } else {
                                    if downloads::target(new).is_some() {
                                        downloads.begin(new.clone(), &db, &emit, &overflow);
                                    } else {
                                        let announced = downloads.finish(new, &emit);
                                        enqueue(
                                            new.clone(),
                                            &folders,
                                            &mut pending,
                                            &overflow,
                                            announced,
                                            true,
                                        );
                                    }
                                }
                                status(&db, None, &emit);
                            }
                            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
                                for path in event.paths {
                                    let _ = db.delete_path(&path);
                                    pending.remove(&path);
                                }
                            }
                            EventKind::Access(_) => {} // Our reads must never feed themselves.
                            _ => {
                                let renamed = matches!(
                                    event.kind,
                                    EventKind::Modify(ModifyKind::Name(RenameMode::To))
                                );
                                for path in event.paths {
                                    if path.is_dir()
                                        && folders.iter().any(|f| path.starts_with(&f.path))
                                    {
                                        // Baseline only the affected subtree. Ordinary directory
                                        // changes must not trigger a scan of an entire drive.
                                        if let Some(folder) =
                                            folders.iter().find(|f| path.starts_with(&f.path))
                                        {
                                            baseline_subtree(&db, folder, &path, &emit);
                                        }
                                    } else {
                                        if !folders.iter().any(|f| path.starts_with(&f.path)) {
                                            continue;
                                        }
                                        if downloads::target(&path).is_some() {
                                            downloads.begin(path, &db, &emit, &overflow);
                                        } else {
                                            let announced = downloads.finish(&path, &emit);
                                            enqueue(
                                                path,
                                                &folders,
                                                &mut pending,
                                                &overflow,
                                                announced,
                                                renamed,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if overflow.swap(false, Ordering::AcqRel) {
                emit(Output::Error("Event backlog: reconciling index; some alerts may be missed / 事件积压，正在校正索引，部分提醒可能遗漏".into()));
                for folder in &folders {
                    generation += 1;
                    scan(&db, folder, generation, &emit);
                }
                pending.clear();
                for download in downloads.active.values() {
                    if download.announced {
                        emit(Output::DownloadFinished {
                            id: download.id,
                            alert: None,
                        });
                    }
                }
                downloads.active.clear();
                deletion_checks = maintenance::DeletionChecks::default();
                status(&db, None, &emit);
            }
            let due: Vec<_> = pending
                .iter()
                .filter(|(_, v)| v.due <= Instant::now())
                .map(|(p, _)| p.clone())
                .collect();
            for path in due {
                let Some(folder) = folders.iter().find(|f| path.starts_with(&f.path)) else {
                    pending.remove(&path);
                    continue;
                };
                if !files::permitted(&path, &folder.path) {
                    if let Some(entry) = pending.remove(&path) {
                        if let Some(id) = entry.announced {
                            emit(Output::DownloadFinished { id, alert: None });
                        }
                    }
                    continue;
                }
                let entry = pending.get_mut(&path).unwrap();
                let record = match files::inspect(&path) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        if let Some(id) = entry.announced {
                            emit(Output::DownloadFinished { id, alert: None });
                        }
                        pending.remove(&path);
                        continue;
                    }
                    Err(_) => {
                        retry(entry);
                        if entry.attempts > 1000 {
                            pending.remove(&path);
                        }
                        continue;
                    }
                };
                let fingerprint = files::fingerprint(&record);
                if entry.announced.is_none()
                    && db
                        .get(&path)
                        .ok()
                        .flatten()
                        .is_some_and(|old| files::fingerprint(&old) == fingerprint)
                {
                    pending.remove(&path);
                    continue;
                }
                if !entry.completed_rename && entry.stable.as_ref() != Some(&fingerprint) {
                    entry.stable = Some(fingerprint);
                    entry.due = Instant::now() + QUIET;
                    continue;
                }
                match files::open_read(&path) {
                    Ok(mut file) => match analyze(&db, folder.id, record, &mut file) {
                        Ok(alert) => {
                            if let Some(id) = entry.announced {
                                emit(Output::DownloadFinished {
                                    id,
                                    alert: alert.map(|mut a| {
                                        a.download_id = Some(id);
                                        Box::new(a)
                                    }),
                                });
                            } else if let Some(alert) = alert {
                                emit(Output::Alert(Box::new(alert)));
                            }
                            pending.remove(&path);
                            status(&db, None, &emit);
                        }
                        Err(e) => {
                            emit(Output::Error(e));
                            retry(entry);
                        }
                    },
                    Err(_) => retry(entry),
                }
                // Bounded retries only while an actual pending file exists.
                if pending.get(&path).is_some_and(|p| {
                    p.first.elapsed() > Duration::from_secs(24 * 3600) || p.attempts > 1000
                }) {
                    pending.remove(&path);
                    emit(Output::Error("File remained busy; use Rebuild to index later / 文件长时间占用，可稍后重建索引".into()));
                }
            }
            // Run after the just-received event and ready download checks. A
            // metadata-only page has at most 64 rows and an 8 ms soft budget.
            if deletion_checks.begin_if_ready(Instant::now()) {
                // One snapshot after a deletion burst, not a settings-list
                // rebuild and queue revalidation for each removed file.
                status(&db, None, &emit);
            }
            if deletion_checks.batch_ready(Instant::now()) {
                match deletion_checks.verifier.step(&db, &folders, |path| {
                    pending.contains_key(path) || downloads.active.contains_key(path)
                }) {
                    Ok(progress) => {
                        if progress.removed > 0 || progress.invalidated > 0 {
                            status(&db, None, &emit);
                        }
                        deletion_checks.after_batch(progress.complete, Instant::now());
                    }
                    Err(error) => {
                        emit(Output::Error(error.to_string()));
                        deletion_checks = maintenance::DeletionChecks::default();
                    }
                }
            }
        }
        drop(watcher);
        drop(db);
        emit(Output::Stopped);
    });
    out
}

fn retry(entry: &mut Pending) {
    entry.attempts += 1;
    // Fast recovery from a final rename while the browser still holds its handle;
    // exponential backoff avoids hammering genuinely busy files.
    entry.due = Instant::now() + Duration::from_millis(100 * (1u64 << entry.attempts.min(8)));
}
fn enqueue(
    path: PathBuf,
    folders: &[Folder],
    pending: &mut HashMap<PathBuf, Pending>,
    overflow: &AtomicBool,
    announced: Option<u64>,
    completed_rename: bool,
) {
    if !folders.iter().any(|f| path.starts_with(&f.path))
        || !path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| names::parse(s).is_some())
    {
        return;
    }
    if pending.len() >= MAX_PENDING {
        overflow.store(true, Ordering::Release);
        return;
    }
    let now = Instant::now();
    pending
        .entry(path)
        .and_modify(|p| {
            p.completed_rename |= completed_rename;
            p.announced = p.announced.or(announced);
            p.due = if p.completed_rename { now } else { now + QUIET };
            p.stable = None;
        })
        .or_insert(Pending {
            due: if completed_rename { now } else { now + QUIET },
            first: now,
            stable: None,
            attempts: 0,
            completed_rename,
            announced,
        });
}
fn status(db: &Db, message: Option<String>, emit: &impl Fn(Output)) {
    match (db.folders(), db.count(), db.setting("language")) {
        (Ok(folders), Ok(files), Ok(language)) => emit(Output::Status(Status {
            folders,
            files,
            language,
            message,
        })),
        _ => emit(Output::Error("Cannot read index / 无法读取索引".into())),
    }
}
fn scan(db: &Db, folder: &Folder, generation: i64, emit: &impl Fn(Output)) {
    if !folder.path.is_dir() {
        emit(Output::Error(format!(
            "Unavailable / 目录不可用: {}",
            folder.path.display()
        )));
        return;
    }
    status(
        db,
        Some(format!("Indexing / 建立索引: {}", folder.path.display())),
        emit,
    );
    let mut complete = true;
    let mut count = 0;
    if let Err(e) = db.conn.execute_batch("BEGIN IMMEDIATE") {
        emit(Output::Error(e.to_string()));
        return;
    }
    for entry in WalkDir::new(&folder.path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !files::is_link(e.path()))
    {
        match entry {
            Ok(entry) if entry.file_type().is_file() => {
                if !files::permitted(entry.path(), &folder.path) {
                    complete = false;
                    continue;
                }
                match files::inspect(entry.path()) {
                    Ok(Some(mut record)) => {
                        if let Ok(Some(previous)) = db.get(entry.path()) {
                            if files::fingerprint(&previous) == files::fingerprint(&record) {
                                record.sample = previous.sample;
                            }
                        }
                        if let Err(e) = db.put(folder.id, &record, generation) {
                            complete = false;
                            emit(Output::Error(e.to_string()));
                        }
                        count += 1;
                    }
                    Ok(None) => {}
                    Err(_) => complete = false,
                }
            }
            Err(_) => complete = false,
            _ => {}
        }
        if count >= 256 {
            if let Err(e) = db.conn.execute_batch("COMMIT; BEGIN IMMEDIATE") {
                emit(Output::Error(e.to_string()));
                let _ = db.conn.execute_batch("ROLLBACK");
                return;
            }
            count = 0;
        }
    }
    if complete {
        if let Err(e) = db.clean_scan(folder.id, generation) {
            emit(Output::Error(e.to_string()));
        }
    } else {
        emit(Output::Error(format!(
            "Partial scan; unreadable files preserved / 部分扫描，保留不可读文件索引: {}",
            folder.path.display()
        )));
    }
    if let Err(e) = db.conn.execute_batch("COMMIT") {
        emit(Output::Error(e.to_string()));
    }
}

fn baseline_subtree(db: &Db, folder: &Folder, path: &std::path::Path, emit: &impl Fn(Output)) {
    if !files::permitted(path, &folder.path) {
        return;
    }
    for entry in WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !files::is_link(e.path()))
    {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_type().is_file() && files::permitted(entry.path(), &folder.path) {
            if let Ok(Some(record)) = files::inspect(entry.path()) {
                if let Err(error) = db.put(folder.id, &record, 0) {
                    emit(Output::Error(error.to_string()));
                }
            }
        }
    }
}

/// Candidates are revalidated against the filesystem before a warning. A stale
/// cached record cannot become an actionable path just because it was in SQLite.
pub fn analyze(
    db: &Db,
    folder_id: i64,
    mut new: Record,
    file: &mut std::fs::File,
) -> Result<Option<Alert>, String> {
    let folders = db.folders().map_err(|e| e.to_string())?;
    let Some(root) = folders.iter().find(|f| f.id == folder_id) else {
        return Ok(None);
    };
    if !files::permitted(&new.path, &root.path) {
        return Ok(None);
    }
    let held = file.metadata().map_err(|e| e.to_string())?;
    let held_modified = held
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string();
    if held.len() != new.size || held_modified != new.modified_ns {
        return Err("File changed before analysis / 文件在分析前已变化".into());
    }
    let candidates = db.candidates(&new).map_err(|e| e.to_string())?;
    if candidates.is_empty() {
        // Most new downloads have no candidate. Index metadata without disk
        // sampling; compute its sample lazily only when it becomes a candidate.
        db.put(folder_id, &new, 0).map_err(|e| e.to_string())?;
        return Ok(None);
    }
    new.sample = Some(files::sample(file).map_err(|e| e.to_string())?);
    let mut best: Option<Alert> = None;
    for cached in candidates {
        let Some(folder) = folders.iter().find(|f| cached.path.starts_with(&f.path)) else {
            continue;
        };
        if !files::permitted(&cached.path, &folder.path) {
            if std::fs::symlink_metadata(&cached.path)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                let _ = db.delete_path(&cached.path);
            }
            continue;
        }
        let Ok(mut old_file) = files::open_read(&cached.path) else {
            continue;
        };
        let Ok(Some(mut old)) = files::inspect(&cached.path) else {
            continue;
        };
        old.sample = if files::fingerprint(&old) == files::fingerprint(&cached) {
            cached.sample.clone()
        } else {
            None
        };
        // Do not spend disk reads on plainly incompatible versions or size.
        let preliminary = matching::classify(&new, &old, false, chrono::Utc::now());
        if old.name.canonical == new.name.canonical
            && matching::size_close(new.size, old.size)
            && old.sample.is_none()
        {
            old.sample = files::sample(&mut old_file).ok();
        }
        db.put(folder.id, &old, 0).map_err(|e| e.to_string())?;
        let alert = matching::classify(
            &new,
            &old,
            new.sample.is_some() && new.sample == old.sample,
            chrono::Utc::now(),
        )
        .or(preliminary);
        if let Some(alert) = alert {
            let rank = |r: Reason| match r {
                Reason::Downloading => 3,
                Reason::SampleMatch => 0,
                Reason::SameVersion => 1,
                Reason::OlderVersion => 2,
            };
            if best
                .as_ref()
                .is_none_or(|current| rank(alert.reason) < rank(current.reason))
            {
                best = Some(alert);
            }
        }
    }
    // A persisted fingerprint deduplicates WRITE/RENAME bursts only for this
    // path + size + write time. A future download, including the same bytes,
    // receives its own warning. There is no global ignore list.
    db.put(folder_id, &new, 0).map_err(|e| e.to_string())?;
    Ok(best)
}
