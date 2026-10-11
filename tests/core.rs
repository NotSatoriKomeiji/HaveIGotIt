use chrono::{TimeZone, Utc};
use haveigotit::{
    db::Db,
    engine,
    files::{self, Record},
    matching::{self, Reason},
    names,
};
use std::{
    fs,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

fn record(name: &str, size: u64) -> Record {
    Record {
        path: name.into(),
        name: names::parse(name).unwrap(),
        size,
        modified_ns: "1".into(),
        age_time: Utc
            .with_ymd_and_hms(2026, 10, 1, 0, 0, 0)
            .unwrap()
            .timestamp(),
        pe_version: None,
        sample: None,
    }
}
fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 11, 0, 0, 0).unwrap()
}

#[test]
fn browser_suffixes_and_case_are_normalized() {
    for name in [
        "SteamSetup (1).EXE",
        "steamsetup（2）.exe",
        "SteamSetup (1) (2).exe",
    ] {
        assert_eq!(names::parse(name).unwrap().canonical, "steamsetup.exe");
    }
}
#[test]
fn compound_extensions_are_preserved() {
    assert_eq!(
        names::parse("App-v2.3.4 (1).tar.gz").unwrap().extension,
        "tar.gz"
    );
    assert!(names::parse("setup.exe.crdownload").is_none());
    assert!(names::parse("archive.zip.part").is_none());
    assert!(names::parse("image.png").is_none());
}
#[test]
fn version_architecture_and_channel_are_not_copy_suffixes() {
    let a = names::parse("VSCodeUserSetup-x64-1.95.3 (1).exe").unwrap();
    let b = names::parse("VSCodeUserSetup-x64-1.96.0.exe").unwrap();
    assert_eq!(a.version.as_deref(), Some("1.95.3"));
    assert_eq!(a.identity, b.identity);
    assert_ne!(
        a.identity,
        names::parse("VSCodeUserSetup-arm64-1.95.3.exe")
            .unwrap()
            .identity
    );
    assert_ne!(
        a.identity,
        names::parse("VSCodeUserSetup-x64-beta-1.95.3.exe")
            .unwrap()
            .identity
    );
    assert_eq!(
        names::parse("Tool (x64).zip").unwrap().canonical,
        "tool (x64).zip"
    );
}
#[test]
fn size_tolerance_has_precise_inclusive_boundary() {
    assert!(matching::size_close(90, 100));
    assert!(matching::size_close(100, 90));
    assert!(!matching::size_close(89, 100));
    assert!(!matching::size_close(0, 0));
    assert!(matching::size_close(u64::MAX, u64::MAX));
}
#[test]
fn sampled_match_warns_about_browser_copy() {
    let a = record("tool.zip", 100);
    let b = record("tool (1).zip", 100);
    assert_eq!(
        matching::classify(&b, &a, true, now()).unwrap().reason,
        Reason::SampleMatch
    );
}
#[test]
fn unknown_versions_and_different_samples_do_not_warn() {
    assert!(matching::classify(
        &record("setup (1).exe", 100),
        &record("setup.exe", 100),
        false,
        now()
    )
    .is_none());
}
#[test]
fn version_conflicts_override_matching_hashes() {
    let old = record("tool-1.2.zip", 100);
    let new = record("tool-1.3.zip", 100);
    assert!(matching::classify(&new, &old, true, now()).is_none());
    let mut old = record("setup.exe", 100);
    let mut new = record("setup (1).exe", 100);
    old.pe_version = Some("1.0.0.0".into());
    new.pe_version = Some("2.0.0.0".into());
    assert!(matching::classify(&new, &old, true, now()).is_none());
}
#[test]
fn same_known_version_with_small_rebuild_is_possible_duplicate() {
    assert_eq!(
        matching::classify(
            &record("tool-1.2 (1).zip", 95),
            &record("tool-1.2.zip", 100),
            false,
            now()
        )
        .unwrap()
        .reason,
        Reason::SameVersion
    );
}
#[test]
fn age_is_strict_calendar_months_and_leap_safe() {
    let old = Utc.with_ymd_and_hms(2024, 11, 30, 12, 0, 0).unwrap();
    let boundary = Utc.with_ymd_and_hms(2025, 2, 28, 12, 0, 0).unwrap();
    assert!(!matching::older_than_three_months(
        old.timestamp(),
        boundary
    ));
    assert!(matching::older_than_three_months(
        old.timestamp(),
        boundary + chrono::Duration::seconds(1)
    ));
    assert!(!matching::older_than_three_months(i64::MAX, now()));
}
#[test]
fn old_different_version_is_separate_age_advisory() {
    let mut old = record("tool-1.2.zip", 100);
    old.age_time = Utc
        .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .unwrap()
        .timestamp();
    assert_eq!(
        matching::classify(&record("tool-1.3.zip", 400), &old, false, now())
            .unwrap()
            .reason,
        Reason::OlderVersion
    );
    assert!(matching::classify(&record("other-1.3.zip", 400), &old, false, now()).is_none());
}
#[test]
fn same_path_is_never_its_own_duplicate() {
    let a = record("setup.exe", 100);
    assert!(matching::classify(&a, &a, true, now()).is_none());
}
#[test]
fn folder_removal_cascades_without_deleting_user_files() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("setup.exe");
    fs::write(&path, b"package").unwrap();
    let db = Db::open(&t.path().join("index.sqlite3")).unwrap();
    let folder = db.add_folder(t.path()).unwrap();
    db.put(folder, &files::inspect(&path).unwrap().unwrap(), 0)
        .unwrap();
    assert_eq!(db.count().unwrap(), 1);
    db.remove_folder(folder).unwrap();
    assert_eq!(db.count().unwrap(), 0);
    assert_eq!(fs::read(path).unwrap(), b"package");
}
#[test]
fn deletion_respects_component_boundaries_and_sql_metacharacters() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let root = Path::new("root");
    let id = db.add_folder(root).unwrap();
    for path in [
        root.join("a_%.zip"),
        root.join("ab.zip"),
        root.join("tree").join("app.zip"),
        root.join("tree2").join("app.zip"),
    ] {
        let mut value = record("app.zip", 100);
        value.path = path;
        db.put(id, &value, 0).unwrap();
    }
    db.delete_path(&root.join("a_%.zip")).unwrap();
    assert_eq!(db.count().unwrap(), 3);
    db.delete_path(&root.join("tree")).unwrap();
    assert_eq!(db.count().unwrap(), 2);
    assert!(db
        .get(&root.join("tree2").join("app.zip"))
        .unwrap()
        .is_some());
}
#[test]
fn scan_generations_remove_missing_rows() {
    let db = Db::open(Path::new(":memory:")).unwrap();
    let id = db.add_folder(Path::new("/root")).unwrap();
    let mut a = record("a.zip", 100);
    a.path = "/root/a.zip".into();
    let mut b = a.clone();
    b.path = "/root/b.zip".into();
    db.put(id, &a, 1).unwrap();
    db.put(id, &b, 2).unwrap();
    db.clean_scan(id, 2).unwrap();
    assert_eq!(db.count().unwrap(), 1);
}
#[test]
fn sampled_hash_is_not_claimed_to_be_full_content_identity() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a.zip");
    let b = t.path().join("b.zip");
    let data = vec![7u8; 1024 * 1024];
    fs::write(&a, &data).unwrap();
    let mut changed = data;
    changed[100_000] = 8;
    fs::write(&b, &changed).unwrap();
    assert_ne!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
    assert_eq!(
        files::sample(&mut files::open_read(&a).unwrap()).unwrap(),
        files::sample(&mut files::open_read(&b).unwrap()).unwrap()
    );
}
#[test]
fn stale_candidate_is_removed_before_notification() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let db = Db::open(Path::new(":memory:")).unwrap();
    let id = db.add_folder(&root).unwrap();
    let old = root.join("setup.exe");
    fs::write(&old, b"same-content").unwrap();
    db.put(id, &files::inspect(&old).unwrap().unwrap(), 0)
        .unwrap();
    fs::remove_file(&old).unwrap();
    let new = root.join("setup (1).exe");
    fs::write(&new, b"same-content").unwrap();
    assert!(engine::analyze(
        &db,
        id,
        files::inspect(&new).unwrap().unwrap(),
        &mut files::open_read(&new).unwrap()
    )
    .unwrap()
    .is_none());
    assert_eq!(db.count().unwrap(), 1);
}
#[cfg(unix)]
#[test]
fn symlink_escape_is_not_authorized() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("setup.exe");
    fs::write(&file, b"data").unwrap();
    let link = root.path().join("setup.exe");
    std::os::unix::fs::symlink(file, &link).unwrap();
    assert!(!files::permitted(
        &link,
        &root.path().canonicalize().unwrap()
    ));
}
#[test]
fn changed_metadata_before_open_is_retried() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let db = Db::open(Path::new(":memory:")).unwrap();
    let id = db.add_folder(&root).unwrap();
    let path = root.join("setup.exe");
    fs::write(&path, b"before").unwrap();
    let record = files::inspect(&path).unwrap().unwrap();
    fs::write(&path, b"after-with-new-size").unwrap();
    assert!(engine::analyze(&db, id, record, &mut files::open_read(&path).unwrap()).is_err());
}

fn wait_for(
    rx: &mpsc::Receiver<engine::Output>,
    timeout: Duration,
    predicate: impl Fn(&engine::Output) -> bool,
) -> engine::Output {
    let deadline = Instant::now() + timeout;
    loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("worker event timed out");
        if let engine::Output::Error(error) = &event {
            panic!("worker error: {error}");
        }
        if predicate(&event) {
            return event;
        }
    }
}
#[test]
fn event_pipeline_temp_rename_ignore_scope_and_remove_folder() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("downloads");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let old = root.join("tool.zip");
    fs::write(&old, b"downloaded package contents").unwrap();
    let (out, rx) = mpsc::channel();
    let tx = engine::start(t.path().join("index.sqlite3"), move |event| {
        out.send(event).unwrap();
    });
    tx.send(engine::Input::Command(engine::Command::Add(root.clone())))
        .unwrap();
    let status = wait_for(
        &rx,
        Duration::from_secs(10),
        |event| matches!(event,engine::Output::Status(s) if s.files==1&&s.message.is_none()),
    );
    let engine::Output::Status(status) = status else {
        unreachable!()
    };
    let id = status.folders[0].id;
    let temp = root.join("tool (1).zip.crdownload");
    let new = root.join("tool (1).zip");
    fs::write(&temp, b"downloaded package contents").unwrap();
    fs::rename(&temp, &new).unwrap();
    let first = wait_for(&rx, Duration::from_secs(15), |event| {
        matches!(event, engine::Output::Alert(_))
    });
    let engine::Output::Alert(first) = first else {
        unreachable!()
    };
    if first.reason == Reason::Downloading {
        wait_for(
            &rx,
            Duration::from_secs(2),
            |event| matches!(event, engine::Output::DownloadFinished { alert: Some(a), .. } if a.reason == Reason::SampleMatch),
        );
    } else {
        assert_eq!(first.reason, Reason::SampleMatch);
    }
    assert_eq!(first.old_file.path, old);
    // A repeated event for the same completed write does not warn again.
    wait_for(
        &rx,
        Duration::from_secs(5),
        |event| matches!(event,engine::Output::Status(s) if s.files==2),
    );
    let repeat = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Data(
        notify::event::DataChange::Any,
    )))
    .add_path(new.clone());
    tx.send(engine::Input::Fs(Ok(repeat))).unwrap();
    let quiet_until = Instant::now() + Duration::from_secs(1);
    while let Ok(event) = rx.recv_timeout(quiet_until.saturating_duration_since(Instant::now())) {
        assert!(
            !matches!(event, engine::Output::Alert(_)),
            "duplicate event raised another alarm"
        );
    }
    // Same path, same bytes, new write time is also a fresh download.
    fs::write(&new, b"downloaded package contents").unwrap();
    wait_for(&rx, Duration::from_secs(15), |event| {
        matches!(event, engine::Output::Alert(_))
    });
    // Ignoring/dismissing the first UI warning does not silence a later copy.
    let next = root.join("tool（2）.zip");
    fs::write(&next, b"downloaded package contents").unwrap();
    wait_for(&rx, Duration::from_secs(15), |event| {
        matches!(event, engine::Output::Alert(_))
    });
    tx.send(engine::Input::Command(engine::Command::Remove(id)))
        .unwrap();
    wait_for(
        &rx,
        Duration::from_secs(5),
        |event| matches!(event,engine::Output::Status(s) if s.files==0&&s.folders.is_empty()),
    );
    assert!(old.exists() && new.exists() && next.exists());
    tx.send(engine::Input::Command(engine::Command::Stop))
        .unwrap();
    wait_for(&rx, Duration::from_secs(5), |event| {
        matches!(event, engine::Output::Stopped)
    });
}

#[test]
fn startup_reconciliation_does_not_reuse_a_previous_scan_generation() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("downloads");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let database = t.path().join("index.sqlite3");
    let path = root.join("gone.zip");
    fs::write(&path, b"old file").unwrap();
    {
        let db = Db::open(&database).unwrap();
        let id = db.add_folder(&root).unwrap();
        db.put(id, &files::inspect(&path).unwrap().unwrap(), 1)
            .unwrap();
    }
    fs::remove_file(&path).unwrap();
    let (out, rx) = mpsc::channel();
    let tx = engine::start(database, move |event| {
        out.send(event).unwrap();
    });
    wait_for(
        &rx,
        Duration::from_secs(10),
        |event| matches!(event,engine::Output::Status(s) if s.files==0&&s.message.is_none()),
    );
    tx.send(engine::Input::Command(engine::Command::Stop))
        .unwrap();
    wait_for(&rx, Duration::from_secs(5), |event| {
        matches!(event, engine::Output::Stopped)
    });
}
