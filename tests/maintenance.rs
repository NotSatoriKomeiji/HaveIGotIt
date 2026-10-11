use haveigotit::{
    db::Db,
    downloads,
    engine::{self, Command, Input, Output},
    files,
    maintenance::{DeletionChecks, Verifier, BATCH_SIZE, DELETE_QUIET},
    matching,
};
use std::{
    collections::VecDeque,
    fs,
    sync::mpsc,
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, Db, haveigotit::db::Folder) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("downloads");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let db = Db::open(&temp.path().join("db")).unwrap();
    let id = db.add_folder(&root).unwrap();
    (temp, db, haveigotit::db::Folder { id, path: root })
}

#[test]
fn one_and_many_delete_events_start_one_sweep_then_return_to_idle() {
    for count in [1, 1000] {
        let mut checks = DeletionChecks::default();
        let start = Instant::now();
        assert!(checks.deadline().is_none());
        for n in 0..count {
            checks.note_delete(start + Duration::from_millis(n));
        }
        let last = start + Duration::from_millis(count - 1);
        assert!(!checks.begin_if_ready(last + DELETE_QUIET - Duration::from_millis(1)));
        let settled = last + DELETE_QUIET;
        assert!(checks.begin_if_ready(settled));
        assert!(!checks.begin_if_ready(settled));
        assert!(checks.batch_ready(settled));
        checks.after_batch(true, settled);
        assert!(checks.deadline().is_none());
        assert!(!checks.begin_if_ready(settled + Duration::from_secs(7 * 86400)));
        checks.note_delete(settled + Duration::from_secs(60));
        assert!(checks.begin_if_ready(settled + Duration::from_secs(60) + DELETE_QUIET));
    }
}

#[test]
fn new_deletions_pause_in_flight_pages_until_the_burst_settles() {
    let mut checks = DeletionChecks::default();
    let now = Instant::now();
    checks.note_delete(now);
    let settled = now + DELETE_QUIET;
    assert!(checks.begin_if_ready(settled));
    checks.after_batch(false, settled);
    checks.note_delete(settled + Duration::from_millis(50));
    assert!(!checks.batch_ready(settled + Duration::from_secs(1)));
    let last = settled + Duration::from_millis(50) + DELETE_QUIET;
    assert!(checks.begin_if_ready(last));
    assert!(checks.batch_ready(last));
    checks.after_batch(true, last);
    assert!(checks.deadline().is_none());
}

#[test]
fn verification_is_bounded_removes_only_missing_rows_and_preserves_user_files() {
    let (_temp, db, folder) = fixture();
    let reference = folder.path.join("keep.zip");
    fs::write(&reference, b"keep this file").unwrap();
    let original = files::inspect(&reference).unwrap().unwrap();
    db.put(folder.id, &original, 1).unwrap();
    let transaction = db.conn.unchecked_transaction().unwrap();
    for n in 0..257 {
        let mut stale = original.clone();
        stale.path = folder.path.join(format!("missing-{n:04}.zip"));
        db.put(folder.id, &stale, 1).unwrap();
    }
    transaction.commit().unwrap();
    let mut verifier = Verifier::default();
    let mut checked = 0;
    let mut removed = 0;
    loop {
        let progress = verifier
            .step(&db, std::slice::from_ref(&folder), |_| false)
            .unwrap();
        assert!(progress.checked <= BATCH_SIZE);
        checked += progress.checked;
        removed += progress.removed;
        if progress.complete {
            break;
        }
        assert!(progress.checked > 0);
    }
    assert_eq!(checked, 258);
    assert_eq!(removed, 257);
    assert_eq!(db.count().unwrap(), 1);
    assert_eq!(fs::read(reference).unwrap(), b"keep this file");
    #[cfg(unix)]
    {
        let outside = folder.path.parent().unwrap().join("outside");
        fs::create_dir(&outside).unwrap();
        let target = outside.join("package.zip");
        fs::write(&target, b"outside package").unwrap();
        std::os::unix::fs::symlink(&outside, folder.path.join("link")).unwrap();
        let mut stale = original.clone();
        stale.path = folder.path.join("link/package.zip");
        db.put(folder.id, &stale, 1).unwrap();
        let progress = Verifier::default().step(&db, &[folder], |_| false).unwrap();
        assert_eq!(progress.removed, 1);
        assert_eq!(fs::read(target).unwrap(), b"outside package");
    }
}

#[test]
fn changed_evidence_is_cleared_without_swallowing_a_queued_download_write() {
    let (_temp, db, folder) = fixture();
    let path = folder.path.join("tool.zip");
    fs::write(&path, b"previous contents").unwrap();
    let mut old = files::inspect(&path).unwrap().unwrap();
    old.sample = Some("stale hash".into());
    old.pe_version = Some("1.0".into());
    db.put(folder.id, &old, 1).unwrap();
    fs::write(&path, b"changed package with a different size").unwrap();
    let mut verifier = Verifier::default();
    let progress = verifier
        .step(&db, std::slice::from_ref(&folder), |_| false)
        .unwrap();
    assert_eq!(progress.invalidated, 1);
    let cached = db.get(&path).unwrap().unwrap();
    assert!(cached.sample.is_none() && cached.pe_version.is_none());
    assert_eq!(files::fingerprint(&cached), files::fingerprint(&old));
    assert_ne!(
        files::fingerprint(&cached),
        files::fingerprint(&files::inspect(&path).unwrap().unwrap())
    );
    assert!(path.is_file());
}

#[test]
fn unavailable_roots_and_active_downloads_are_preserved() {
    let (_temp, db, folder) = fixture();
    let path = folder.path.join("tool.zip");
    fs::write(&path, b"contents").unwrap();
    let record = files::inspect(&path).unwrap().unwrap();
    db.put(folder.id, &record, 1).unwrap();
    fs::remove_file(&path).unwrap();
    let mut verifier = Verifier::default();
    assert_eq!(
        verifier
            .step(&db, std::slice::from_ref(&folder), |p| p == path)
            .unwrap()
            .removed,
        0
    );
    assert_eq!(db.count().unwrap(), 1);
    fs::remove_dir(&folder.path).unwrap();
    let mut verifier = Verifier::default();
    assert_eq!(
        verifier
            .step(&db, std::slice::from_ref(&folder), |_| false)
            .unwrap()
            .removed,
        0
    );
    assert_eq!(db.count().unwrap(), 1);
}

#[test]
fn missing_or_changed_old_files_withdraw_visible_and_queued_alarms() {
    for replacement in 0..3 {
        let (_temp, db, folder) = fixture();
        let old_path = folder.path.join("tool.zip");
        let new_path = folder.path.join("tool (1).zip");
        fs::write(&old_path, b"same contents").unwrap();
        fs::write(&new_path, b"same contents").unwrap();
        let old = files::inspect(&old_path).unwrap().unwrap();
        let new = files::inspect(&new_path).unwrap().unwrap();
        let alert = matching::classify(&new, &old, true, chrono::Utc::now()).unwrap();
        db.put(folder.id, &old, 1).unwrap();
        assert!(downloads::alert_valid(
            &alert,
            std::slice::from_ref(&folder)
        ));
        if replacement == 1 {
            fs::write(&old_path, b"new package of a different size").unwrap();
        } else {
            fs::remove_file(&old_path).unwrap();
            if replacement == 2 {
                fs::create_dir(&old_path).unwrap();
            }
        }
        // The DB still contains the old record: presentation does not depend on a sweep.
        assert_eq!(db.count().unwrap(), 1);
        assert!(!downloads::alert_valid(
            &alert,
            std::slice::from_ref(&folder)
        ));
        let mut current = Some(alert.clone());
        let mut queued = VecDeque::from([alert]);
        assert!(downloads::prune_alerts(
            &mut current,
            &mut queued,
            &[folder]
        ));
        assert!(current.is_none() && queued.is_empty());
        assert!(new_path.is_file());
    }
}

#[test]
fn real_bulk_delete_coalesces_into_one_update_and_no_idle_rescans() {
    let (temp, db, folder) = fixture();
    // Worker owns its own connection; no preconfigured watcher/rows yet.
    db.remove_folder(folder.id).unwrap();
    let mut paths = Vec::new();
    for n in 0..16 {
        let path = folder.path.join(format!("tool-{n}.zip"));
        fs::write(&path, b"package").unwrap();
        paths.push(path);
    }
    let (out, rx) = mpsc::channel();
    let tx = engine::start(temp.path().join("db"), move |event| {
        let _ = out.send(event);
    });
    tx.send(Input::Command(Command::Add(folder.path.clone())))
        .unwrap();
    loop {
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            Output::Status(s) if s.files == 16 && s.message.is_none() => break,
            Output::Error(e) => panic!("{e}"),
            _ => {}
        }
    }
    let temporary = folder.path.join("Unconfirmed 123.crdownload");
    fs::write(&temporary, b"").unwrap();
    fs::remove_file(&temporary).unwrap();
    assert!(
        rx.recv_timeout(Duration::from_millis(1200)).is_err(),
        "unindexed temporary cleanup triggered a sweep"
    );
    for path in paths {
        fs::remove_file(path).unwrap();
    }
    match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        Output::Status(s) => assert_eq!(s.files, 0),
        Output::Error(e) => panic!("{e}"),
        _ => panic!("unexpected alarm"),
    }
    assert!(
        rx.recv_timeout(Duration::from_millis(1500)).is_err(),
        "burst triggered extra updates"
    );
    assert!(folder.path.is_dir());
    tx.send(Input::Command(Command::Stop)).unwrap();
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Output::Stopped
    ));
}
