# v0.1.0 validation — 2026-10-11

This is a first trial build. The automated portable-core checks passed; Windows desktop runtime behavior has not been verified on a real Windows machine.

## Completed checks

| Check | Result |
|---|---|
| `cargo test --locked` on Linux | 36 tests passed, including real filesystem events, deletion coalescing and latency regression checks |
| `cargo fmt --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| Windows x64 GNU target, all-targets Clippy with warnings denied | Passed |
| English/Chinese optimized Windows x64 builds | Cross-compiled and linked; Win32 resource/manifest included |
| GitHub workflow and PowerShell release script | Included; have not been executed on GitHub/PowerShell here |
| Windows GUI runtime / RAM and CPU benchmark | Not run; no numerical resource-use claims |

Build environment: Rust stable 1.99.0; GCC MinGW x64 13.2; locked dependencies in `Cargo.lock`. The current runtime cannot start the attempted Wine graphics test because local IPC sockets are unavailable. That is an environment limitation, not evidence that the Windows UI passed or failed.

The automated tests cover ASCII/full-width browser copy suffixes; compound extensions and temporary-download exclusion; architecture/channel preservation; exact size boundary and overflow safety; version conflicts overriding matching hashes; unknown versions; same-version rebuild hints; strict three-calendar-month/leap-end-of-month age; separate old-version advisories; self-match exclusion; cascading folder removal while preserving user files; SQL wildcard characters and path component boundaries; scan-generation cleanup; stale-candidate deletion; limited-hash false-equality semantics; symlink escape; changed metadata; restart reconciliation; real temporary-file rename; repeated-event suppression; another write to the same path; another browser-suffixed download; removal of authorization.

## Windows acceptance before a stable public release

Use disposable test directories with test archives, never an important file collection.

| Scenario | Expected behavior |
|---|---|
| First launch of each language package with no saved settings | Corresponding language, no authorized folders by default |
| Add Downloads and an installer storage folder | Baseline index, visible count, no historical duplicate-popup storm |
| Download/copy `tool.zip` as `tool (1).zip` and `tool（2）.zip` | One alarm per download: early same-name hint where available, otherwise a verified final result |
| Ignore then repeat, including writing again to the same path | New completed write may warn again; repeated FS events do not |
| Named `.crdownload` / `.part`, initially zero bytes | Same-name hint before completion; explicitly unverified; no incomplete row in the index |
| Temporary name renamed to `.exe` / `.zip` | Verify immediately when Windows releases the writer; update the same popup, never reopen an ignored one |
| Browser sends separate RenameFrom and RenameTo events | Preserve the session and current alarm across the transition |
| Cancel a hinted download, then repeat at the same path | Withdraw canceled hint; new download receives a new session and may warn again |
| Random `Unconfirmed 123.crdownload` or a near-instant download | Final-name verification; no guarantee of pre-completion presentation |
| `tool-1.2.zip` vs `tool-1.3.zip` | No duplicate warning; age advisory only if old file is over three months |
| Versions equal, ≤10% size difference | Possible-duplicate hint; never an invented “98%” score |
| Completed versions unknown and sampled bytes differ | No verified duplicate; withdraw any visible same-name hint |
| Old candidate deleted while app runs | Drop its index; no stale location action |
| Single indexed-file deletion or a bulk deletion | One verification sweep after a one-second quiet period; no recurring idle verification |
| Old file disappears/changes while an alarm waits | Recheck before showing; withdraw visible/queued stale alarms |
| Unindexed temporary-file cleanup | No index verification sweep |
| Large deletion sweep while downloading | Bounded 64-record pages/8 ms soft budget; yield between pages; no hash reads or user-file deletion |
| Remove a root with a visible alarm | Clear its rows and related queued/visible alarms; files remain |
| Open Location for long/Chinese/space-containing paths | Explorer selects the old file; does not launch the file |
| Close/minimize settings; right-click tray; restart Explorer | App stays in tray; menu works; tray re-registers after Explorer restart |
| 100% / 125% / 150% / 200% DPI; multiple monitors and taskbar edges | Buttons remain usable, content scrolls, alarm stays inside the monitor work area |
| Click inside the native folder picker or leave tray menu open while a download completes | Background outputs continue processing safely |
| Startup with `--background` | Hidden settings, normal monitoring and tray |
| Offline removable drive / permission-denied subtree | Report limitation, preserve unreadable index; Rebuild reconnects |
| Ordinary copied files and directory imports | No browser-specific claim; directory baseline may suppress imports |

For performance, record private working set, handle count and idle CPU in Windows Task Manager after indexing a representative collection. Separately measure initial scanning and a 10+ GiB download. Do not substitute EXE file size for process RAM or claim zero memory use.


## Explorer-location hotfix

The user supplied a Windows screenshot showing the generic location-opening error. It did not include an HRESULT, so the screenshot alone cannot establish the exact failing Shell call.

The code passed Rust-canonicalized `\\?\C:\...` filesystem paths directly to the Shell namespace parser. The hotfix converts drive and UNC prefixes to Shell display form without losing UTF-16 characters, explicitly balances COM initialization for the operation, and falls back to opening only the parent directory when selection fails. It never opens the installer itself. If both operations fail, the error includes the selection HRESULT and folder-opening code.

Four additional regression tests cover canonical drive paths, Chinese/space/non-BMP characters, lossless UTF-16 handling, UNC roots, rejected device namespace paths and embedded NULs. These tests verify the conversion; actual Explorer selection/fallback still needs a Windows trial.

## Early-warning and latency fix

The old code rejected temporary names and imposed two two-second waits after the final event. The revised path issues a read-free, indexed name hint for recognizable temporary files, immediately checks final renames, and uses two 150 ms quiet checks for ordinary final-name writes. No browser API or new dependency was added. Exact/prefix index deletion replaces the previous full-table substring filter; downloads without candidates skip hash reads.

Five added tests cover zero-byte pre-completion hints; preservation of counters/versions/architecture/compound extensions; live authorized old candidates; failed-verification withdrawal; ignored-alarm non-resurrection; reused destination sessions; cancellation; root revocation; rename-transition visibility; and indexed lookup with 100,000 rows.

One Linux debug test run on this host measured temporary creation to worker alarm at **0.344 ms**, completed rename to worker verification at **0.830 ms**, and indexed name lookup over 100,000 records at **0.017 ms median / 0.027 ms p95** across 100 queries. These are a small synthetic sample with local files/cache, not a Windows popup benchmark, real browser throughput measurement, or latency guarantee. Regression event waits have a two-second deadline to catch the former four-second delay. Reproduce using `cargo test --locked -- --nocapture`.

Windows acceptance must still measure browser-start to visible-popup latency, using slow and fast downloads in Chrome/Edge/Firefox, including random temporary names and final-name downloaders. Initial indexing/rebuild remains serialized; do not measure steady-state latency before it finishes.

## Final UI and deletion-validation checks

Settings no longer contain the two-buttons/your-decision sentence or close-to-tray/right-click-exit footer. The footer still shows the file/folder counts and errors; layout reserves less footer space. Tray behavior remains documented in the README.

Seven additional tests verify single and 1,000-event deletion-burst coalescing; no recurring deadline after completion; new deletions during an in-flight sweep; bounded paging over 258 entries; removal of missing index records while preserving real files; stale-evidence invalidation without swallowing queued writes; unavailable-root and active-download retention; live/queued alarm withdrawal for missing or changed old packages; and one update after a real 16-file bulk deletion. Linux tests and Windows target compilation/lints passed; visible native layout and Explorer-specific deletion grouping still require a Windows trial.

Resource behavior is bounded by implementation, not a measured Windows RAM promise. No extra thread/dependency is added. Metadata batches avoid full-index materialization, full-directory traversal, hash/resource reads and forced working-set trimming. SQLite idle cache release is opportunistic; it cannot guarantee a particular total process working set.
