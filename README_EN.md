# HaveIGotIt?

**Ever downloaded the same installer twice? Your PC remembers. Now you do too.**

A small Windows tray app that remembers installers and archives in folders you explicitly authorize. Named browser temporary files can trigger an early, unverified same-name hint; completed files undergo evidence checks. The corner alarm has exactly two actions: **Open Location** and **Ignore**.

It is an alarm, not a cleaner. Decisions stay with you.

[简体中文说明](README_CN.md) · [Architecture and matching rules](docs/ARCHITECTURE.md) · [Validation](docs/VALIDATION.md)

## Start

1. Extract the English or Chinese Windows x64 ZIP and run `HaveIGotIt.exe`.
2. Click **Add folder** and authorize your Downloads folder and any installer/archive storage folders.
3. Wait for the initial index. Close the window to leave the app in the tray.
4. Start a download in an authorized folder. A recognizable `.crdownload` / `.part` name can trigger a same-name hint before completion. Size, version and content are explicitly unverified. Completion updates that hint in place, or withdraws it if verification finds no match.
5. **Ignore** dismisses only this download's alarm. The next download can warn again, including the same bytes or the same destination path after a new write.

Double-click the tray icon for settings. Right-click it to exit. Add/remove folders at any time; removing a folder removes its database records, not its files. The **Rebuild index** button repairs the index and reconnects watches after a drive becomes available again.

English and Chinese packages contain the same core with different first-launch language defaults. Both can switch language in settings. They share one per-user index and saved language preference. Do not run both together.

## What qualifies

- Supported extensions: `.exe`, `.msi`, `.msix`, `.msixbundle`, `.zip`, `.rar`, `.7z`, `.iso`, `.cab`, `.tar.gz`, `.tar.xz`, `.tar.bz2`.
- No required filename keyword: `abc.zip` is eligible too. An `.exe` is not assumed to be an installer based on its extension alone.
- Case differences and trailing numeric copy counters such as ` (1)`, `（2）`, or repeated counters are normalized. Product version, architecture, channel and package-format text remain significant.
- The size difference must be at most 10% of the larger file. Matching name alone is insufficient: sampled content must agree, or comparable known versions must agree.
- Known filename-version or EXE-resource-version conflicts override duplicate matching. Signer identity is not used as a version number.
- Completed unversioned files with different sampled content do not receive a duplicate result. Any visible early same-name hint is withdrawn; it is never treated as verified evidence.
- Sampling reads at most three 64 KiB regions plus metadata, and is **not** a full-file equality check. The UI says “possible duplicate.”

The optional-looking age line is included automatically in a qualifying alarm when the old file's earliest available filesystem date is **strictly over three calendar months old**. For a confidently matching filename identity with different known versions, it can show a separate **older package advisory**, never a duplicate warning. It does not claim whether the new version is an upgrade or downgrade. There is no daily age popup or automatic cleanup.

## Event-driven operation

Idle workers block on notifications and messages. There is no periodic whole-folder polling or full-file hashing. A small OS filesystem listener must remain alive to learn that files appeared: memory use is **not zero** and has not been benchmarked on Windows.

Initial authorization/startup, manual rebuild, and recovery from lost events reconcile the index. Ordinary file changes query indexed candidates and update only affected records. Directory imports/renames baseline only the affected subtree.

Removing indexed files or their containing directories immediately removes the affected rows and schedules **one** metadata-only validation sweep after **one second of quiet**. Consecutive deletion events share that sweep. It uses keyset pages of at most **64 records**, an **8 ms soft budget**, and **one-second pauses between pages**; completion returns to indefinite idle waiting. No recurring validation timer, new thread, dependency, full-folder walk or hash recomputation is added. Unindexed temporary-file deletions do not trigger a sweep.

Missing files lose their index entries only. Unavailable roots and permission failures retain records, but cannot provide alarm candidates. Changed files lose stale hash/version evidence without advancing the completed-write fingerprint and suppressing a pending download. Page allocations are dropped after each batch; SQLite can release idle cache memory at the end, while freed database pages are reused. No forced working-set trimming or expensive VACUUM is performed.

Every queued alarm checks the old file again before presentation, including existence, file type, authorization and size/write time. Missing or changed old files withdraw visible/queued alarms. Filesystem events do not identify the human actor or an Explorer deletion-operation boundary: deletions separated by more than a second are treated as a new burst. Startup scanning repairs changes made while the app was stopped.

Temporary files are never indexed, read or hashed. Names such as `tool (1).exe.crdownload` or `tool（2）.zip.part` are looked up immediately through the existing name index. One download-session ID follows the temporary-to-final rename: completion never resurrects an ignored alarm. Browser final renames attempt verification immediately, removing the previous fixed four-second delay. Ordinary final-name writes retain two 150 ms quiet checks and Windows' writer-excluding read handle. Busy files retry after 200 ms, then use exponential backoff up to 25.6 seconds. Files without candidates are indexed without sampled reads.

Random temporary names such as `Unconfirmed 123.crdownload` do not reveal the target. Very small files may also complete before the OS delivers an event. Filesystem watching cannot guarantee a pre-completion warning for every download; browser download-event integration would be needed to observe the earlier browser stage. Copying eligible files can also cause alarms; the app does not identify browser intent.

## Local data and limits

Configuration and the SQLite index live in `%LOCALAPPDATA%\HaveIGotIt\index.sqlite3`. The app has no network client, browser integration, telemetry, upload, file execution, download cancellation or file-deletion feature.

Subfolders are included; child symlinks, junctions and other reparse points are skipped. Local folders only; online-only cloud placeholders and network shares are outside this first version. Unavailable folders remain configured. Permission failures are reported and prevent a partial scan from purging unreadable records. Candidates are checked again before alarming and before opening Explorer.

During extreme event storms the queue is bounded and the index is reconciled; some alarms may be missed. At most 128 database candidates are examined per new file and the visible alarm queue is bounded. Whole-directory imports may be treated as a baseline. Initial scans of whole drives can take time. If a download manager keeps its final name but releases all write handles during a long pause, readiness is a heuristic; browser temporary-name downloads are the best-supported flow.

Target: Windows 10/11 x64. The supplied binary is an unsigned, cross-compiled **first trial build**, not a claim of completed Windows UI testing. See validation before publishing it as a stable release.

## Build from source

Install current stable Rust and the Visual Studio C++ Build Tools with a Windows SDK on Windows:

```powershell
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
# Chinese first-launch default:
cargo build --locked --release --features zh-release
# Or build both ZIPs:
powershell -NoProfile -File scripts/build-release.ps1
```

The binary is `target\release\haveigotit.exe`. `--background` starts with settings hidden; no startup entry is created automatically. If desired, create your own Startup-folder shortcut with that flag.

CI runs portable-core and Windows tests and builds both language packages. Tags named `v*` create a GitHub draft release after tests pass; review it and publish it yourself. Nothing was uploaded to GitHub by this deliverable.

MIT license; bundled third-party license notices are under `licenses/`.


### Explorer-location hotfix

The current package normalizes verbatim canonical paths before passing them to Explorer and preserves Unicode filenames. If selecting the file fails, it attempts to open only the parent directory. Both failures include native error codes. Exit the old tray process before launching this updated build; the existing index and folder configuration are retained.
