use haveigotit::{
    db::Db,
    downloads,
    engine::{self, Command, Input, Output},
    files,
    matching::Reason,
    names,
};
use std::{
    collections::VecDeque,
    fs,
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

fn receive(rx: &mpsc::Receiver<Output>, predicate: impl Fn(&Output) -> bool) -> Output {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("event exceeded 2 seconds");
        if let Output::Error(error) = &event {
            panic!("{error}");
        }
        if predicate(&event) {
            return event;
        }
    }
}

#[test]
fn temporary_targets_preserve_browser_counters_versions_and_formats() {
    for name in [
        "Tool (1).exe.crdownload",
        "Tool（2）.tar.gz.part",
        "Tool-1.2-x64.zip.CRDOWNLOAD",
    ] {
        let target = downloads::target(Path::new(name)).unwrap();
        assert_eq!(
            names::parse(target.to_str().unwrap()),
            names::parse(name.rsplit_once('.').unwrap().0)
        );
    }
    for name in [
        "Unconfirmed 123.crdownload",
        "random.part",
        "notes.txt.part",
        "Tool.zip",
        "tool.zip.crdownload.part",
    ] {
        assert!(downloads::target(Path::new(name)).is_none());
    }
}

#[test]
fn zero_byte_preview_is_explicitly_unverified_and_requires_a_live_candidate() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let db = Db::open(&root.join("db")).unwrap();
    let id = db.add_folder(&root).unwrap();
    let old = root.join("tool-1.2-x64.zip");
    fs::write(&old, b"old package").unwrap();
    let record = files::inspect(&old).unwrap().unwrap();
    db.put(id, &record, 1).unwrap();
    let temporary = root.join("tool-1.2-x64（2）.zip.crdownload");
    fs::write(&temporary, b"").unwrap();
    let final_path = downloads::target(&temporary).unwrap();
    let preview = downloads::preview(&db, &temporary, &final_path)
        .unwrap()
        .unwrap();
    assert_eq!(preview.reason, Reason::Downloading);
    assert_eq!(preview.new_file.size, 0);
    assert!(preview.new_file.sample.is_none() && preview.new_file.pe_version.is_none());
    assert_eq!(db.count().unwrap(), 1); // No incomplete files in the index.
    for name in [
        "tool-1.3-x64 (1).zip.crdownload",
        "tool-1.2-arm64 (1).zip.crdownload",
    ] {
        let path = root.join(name);
        fs::write(&path, b"").unwrap();
        assert!(
            downloads::preview(&db, &path, &downloads::target(&path).unwrap())
                .unwrap()
                .is_none()
        );
    }
    let outside = tempfile::tempdir().unwrap();
    let unauthorized = outside.path().join("tool-1.2-x64 (3).zip.part");
    fs::write(&unauthorized, b"").unwrap();
    assert!(downloads::preview(
        &db,
        &unauthorized,
        &downloads::target(&unauthorized).unwrap()
    )
    .unwrap()
    .is_none());
    fs::remove_file(&old).unwrap();
    assert!(downloads::preview(&db, &temporary, &final_path)
        .unwrap()
        .is_none());
}

#[test]
fn partial_download_warns_before_completion_and_ignore_is_once_per_download() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("downloads");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    fs::write(root.join("tool.zip"), b"complete contents").unwrap();
    let (out, rx) = mpsc::channel();
    let tx = engine::start(t.path().join("db"), move |e| {
        let _ = out.send(e);
    });
    tx.send(Input::Command(Command::Add(root.clone()))).unwrap();
    let Output::Status(status) = receive(
        &rx,
        |e| matches!(e,Output::Status(s) if s.files==1 && s.message.is_none()),
    ) else {
        unreachable!()
    };
    let temporary = root.join("tool（2）.zip.crdownload");
    let final_path = downloads::target(&temporary).unwrap();
    let start = Instant::now();
    fs::write(&temporary, b"").unwrap();
    let Output::Alert(first) = receive(&rx, |e| matches!(e, Output::Alert(_))) else {
        unreachable!()
    };
    eprintln!(
        "temporary create -> early worker alert: {:?}",
        start.elapsed()
    );
    assert!(temporary.exists() && !final_path.exists());
    assert_eq!(first.reason, Reason::Downloading);
    let id = first.download_id.unwrap();
    // Simulate the user's Ignore: no current or queued UI alarm.
    let mut current = None;
    let mut queue = VecDeque::new();
    fs::write(&temporary, b"complete contents").unwrap();
    let renamed = Instant::now();
    fs::rename(&temporary, &final_path).unwrap();
    let Output::DownloadFinished {
        id: finished,
        alert: Some(verified),
    } = receive(&rx, |e| {
        // Even after writes/rename, another new alarm would violate Ignore.
        assert!(!matches!(e, Output::Alert(_)), "same download warned twice");
        matches!(e, Output::DownloadFinished { .. })
    })
    else {
        panic!("verification missing")
    };
    eprintln!("completed rename -> verification: {:?}", renamed.elapsed());
    assert_eq!(id, finished);
    assert_eq!(verified.reason, Reason::SampleMatch);
    downloads::finish_alerts(&mut current, &mut queue, id, Some(&verified));
    assert!(current.is_none() && queue.is_empty());
    receive(&rx, |e| matches!(e,Output::Status(s) if s.files==2));
    // Reuse exactly the same temporary/final path: a fresh download gets a new ID.
    fs::write(&temporary, b"").unwrap();
    let Output::Alert(second) = receive(&rx, |e| matches!(e, Output::Alert(_))) else {
        unreachable!()
    };
    assert_ne!(second.download_id, first.download_id);
    let id2 = second.download_id.unwrap();
    current = Some(*second);
    // A delayed result from the old download cannot replace this new hint.
    downloads::finish_alerts(&mut current, &mut queue, id, Some(&verified));
    assert_eq!(current.as_ref().unwrap().download_id, Some(id2));
    fs::remove_file(&temporary).unwrap();
    receive(
        &rx,
        |e| matches!(e, Output::DownloadFinished { id, alert: None } if *id==id2),
    );
    tx.send(Input::Command(Command::Stop)).unwrap();
    receive(&rx, |e| matches!(e, Output::Stopped));
    assert!(final_path.exists() && root.join("tool.zip").exists());
    assert_eq!(status.folders.len(), 1);
}

#[test]
fn visible_hint_updates_in_place_and_clears_on_failed_verification() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let db = Db::open(&root.join("db")).unwrap();
    let folder = db.add_folder(&root).unwrap();
    let old = root.join("tool.zip");
    let temp = root.join("tool (1).zip.part");
    fs::write(&old, b"old").unwrap();
    fs::write(&temp, b"").unwrap();
    db.put(folder, &files::inspect(&old).unwrap().unwrap(), 1)
        .unwrap();
    let mut hint = downloads::preview(&db, &temp, &downloads::target(&temp).unwrap())
        .unwrap()
        .unwrap();
    hint.download_id = Some(4);
    let mut current = Some(hint.clone());
    let mut queued = VecDeque::from([hint.clone()]);
    fs::rename(&temp, downloads::target(&temp).unwrap()).unwrap();
    // Status arriving between Windows RenameFrom/RenameTo keeps the alarm valid.
    assert!(downloads::alert_valid(&hint, &db.folders().unwrap()));
    assert!(!downloads::alert_valid(&hint, &[]));
    hint.reason = Reason::SampleMatch;
    hint.new_file.path = downloads::target(&temp).unwrap();
    assert!(!downloads::finish_alerts(
        &mut current,
        &mut queued,
        4,
        Some(&hint)
    ));
    assert_eq!(current.as_ref().unwrap().reason, Reason::SampleMatch);
    assert_eq!(queued.front().unwrap().reason, Reason::SampleMatch);
    assert!(downloads::finish_alerts(&mut current, &mut queued, 4, None));
    assert!(current.is_none() && queued.is_empty());
}

#[test]
fn indexed_preview_query_handles_a_large_collection() {
    let t = tempfile::tempdir().unwrap();
    let db = Db::open(&t.path().join("db")).unwrap();
    let folder = db.add_folder(t.path()).unwrap();
    db.conn.execute("INSERT INTO files(path_key,path,folder_id,canonical,identity,extension,size,modified_ns,age_time)
        WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000)
        SELECT 'p'||x,'p'||x,?1,'package-'||x||'.zip','package-'||x||'.zip','zip',42,'1',1 FROM n", [folder]).unwrap();
    let name = names::parse("package-75000 (1).zip").unwrap();
    let mut times = Vec::new();
    for _ in 0..100 {
        let start = Instant::now();
        assert_eq!(
            db.named_candidates(&name, Path::new("new.zip"))
                .unwrap()
                .len(),
            1
        );
        times.push(start.elapsed());
    }
    times.sort();
    eprintln!(
        "100,000 indexed files, name lookup median {:?}, p95 {:?}",
        times[50], times[95]
    );
    let mut query = db.conn.prepare("EXPLAIN QUERY PLAN SELECT path FROM files WHERE canonical=?1 AND path_key<>?2 LIMIT 128").unwrap();
    let plan = query
        .query_map([&name.canonical, &"new.zip".to_owned()], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        plan.iter()
            .any(|s| s.contains("SEARCH") && s.contains("files_name_size")),
        "{plan:?}"
    );
}
