# v0.1.0 architecture and behavior contract

Rust + Win32 controls and tray + `notify` + bundled SQLite. No WebView, egui redraw loop, browser extension or Windows service. The project is one small package with a portable library and a Windows-only UI binary.

## Modules

| File | Responsibility |
|---|---|
| `names.rs` | Extension allowlist, terminal numeric-copy suffixes, explicit filename version and preserved package identity |
| `downloads.rs` | Named temporary-file hints, read-free candidate lookup and in-place session updates |
| `files.rs` | Read-only metadata, whitelist/reparse checks, read handles and sampled SHA-256 |
| `shell_paths.rs` | Lossless UTF-16 conversion of canonical filesystem paths to Explorer display paths |
| `version.rs` | EXE fixed product-version resources through Win32; never loads or executes the executable |
| `db.rs` | SQLite schema, indexed candidate queries, folder cascade, component-safe subtree deletion |
| `matching.rs` | Version conflict gate, 10% size bound, evidence categories and strict calendar-month age |
| `maintenance.rs` | Deletion-burst coalescing, bounded metadata-only verification and idle cache release |
| `engine.rs` | One worker owning DB and watcher, scans, per-file pending state, event reconciliation |
| `ui.rs` | Native settings, folder chooser, tray, bounded alarm queue, DPI layout, Explorer selection |

UI commands and filesystem events enter one bounded queue. The worker blocks indefinitely with no pending files; only pending paths create short readiness deadlines. The UI receives outputs via a bounded mailbox and posted Win32 messages. Modal UI operations release the state borrow so nested message loops can continue draining outputs.

## New file path

1. Reject events outside enabled roots. Recognize supported final names plus named `.crdownload` / `.part` targets.
2. A live named temporary file triggers an immediate indexed canonical-name lookup. Revalidate old-path metadata and authorization, without reading either file's content. Emit a `Downloading` hint explicitly marking final size/version/content unverified. A zero-byte temporary file is enough; it is never indexed.
3. Track each active download with a monotonically assigned session ID, separate from paths. ASCII/full-width filename counters normalize for lookup, but exact destination paths keep simultaneous downloads distinct. Windows' separate RenameFrom/RenameTo events preserve the session until the final path arrives; combined rename events are also supported.
4. Final renames immediately try verification. Ordinary final-name writes wait 150 ms of quiet, then require the same size/write-time fingerprint after another 150 ms. A stored completed fingerprint suppresses repeated events, not future downloads.
5. Open using `FILE_SHARE_READ`, excluding simultaneous writers. Busy files retry at 200 ms and use exponential backoff up to 25.6 seconds; limits remain 1,000 attempts / one day.
6. Recheck held-handle metadata and query indexed candidates first. No candidates: persist metadata without hashing. Otherwise sample size + first/centre/last regions; revalidate old paths and refresh stale samples lazily. Prefer sampled matches, then same-version matches, then age advisories.
7. Persist the new fingerprint. If an early hint was announced, send `DownloadFinished` for that ID: replace an existing visible/queued hint, or withdraw it when verification finds no match. Never append an alarm for an already dismissed ID. Without an early hint, send at most one best-candidate alarm.
8. Delete/cancel and revoked roots clear the relevant sessions and hints. There is no idle session-pruning timer; capacity-pressure cleanup is bounded by the 4,096-session limit.

If a future download has the same normalized name or even the same content, it can warn again: the path and/or write time is different. Re-downloading to the same existing path does not compare the file with its own previous contents; another existing copy must remain to serve as the old candidate.

## Schema

| Table | Key and fields |
|---|---|
| `folders` | `id`, authorized canonical `path`, unique Windows-case-folded `path_key` |
| `files` | unique `path_key`, `path`, `folder_id` FK cascade, `canonical`, `identity`, `extension`, `name_version`, `size`, `modified_ns`, `age_time`, `pe_version`, cached `sample`, `scan_generation` |
| `settings` | key/value; saved language |

No ignore table, no seven-day missing-path retention. Root removals delete all child rows through FK cascade. Delete events use exact keys and a binary-collation component-prefix range through the path index, avoiding both full-table `substr` scans and SQL `LIKE` wildcard mistakes.

Indexes: `(canonical,size)`, `(identity)`, primary `path_key`; queries are capped at 128 candidates. SQLite uses WAL, one connection on the worker, a roughly 1 MiB page-cache target and batched initial-scan commits. Sample hashes are computed lazily, not for every indexed archive.

Startup/manual/recovery scans mark seen rows with an increasing generation. Only a completed scan removes unseen rows. Partial scans report their limitations and retain unreadable rows. Directory imports baseline only the affected subtree. Normal delete/move events update affected rows without a whole-root scan. Candidates are lazily revalidated even if a delete event was missed.

## Deletion-triggered validation

There is no recurring idle timer. A Remove event that actually deletes indexed rows updates them immediately and resets a one-second quiet deadline. The resulting burst starts one verification sweep and one initial status refresh, rather than one sweep/status rebuild for every removed file. Unindexed temporary-file cleanup does not schedule a sweep.

The sweep captures the current maximum path key and pages through `(last_key, upper_key]` using the existing path index. Each page holds at most 64 records, uses an 8 ms soft processing budget and groups writes in one transaction. A single filesystem metadata call cannot be preempted by that budget. One-second pauses between pages allow download events/UI commands to run. Active pending downloads are skipped. Deletions during a sweep pause it; the next quiet-separated burst resets the cursor so earlier keys cannot be missed. Once complete, the deadline is cleared and the worker returns to blocking `recv()`.

Only metadata is checked, without hashes or PE resource reads. Definitely missing/ineligible records are removed; unreadable entries or unavailable roots are retained. For changed files, clear stale sample/PE evidence while preserving the last completed fingerprint: silently advancing it here could swallow a queued write event. Per-page allocations are released, SQLite `PRAGMA shrink_memory` releases idle cache allocations, and freed database pages are reused. No periodic VACUUM, database copy, forced working-set trimming, filesystem deletion or new worker thread is introduced.

The UI independently checks live metadata and authorization before showing each alarm and on status changes; a queued stale record cannot bypass this gate. Missing/changed old files withdraw current and queued alarms, and clicking a now-unavailable location silently dismisses the stale alarm. The watcher cannot distinguish an Explorer action from an application delete or provide an exact batch-operation ID; a one-second event gap defines the grouping.

## Matching policy

The filename canonical form lowercases, recognizes compound extensions and removes only trailing ASCII-number parentheses. No edit-distance fuzzy search in v0.1: unrelated similarly named packages should not trigger based on a guessed percentage.

The filename identity replaces a confidently recognized dotted or `v`-prefixed version token and otherwise preserves the filename. Architecture, product flavor, channel, build qualifiers and packaging remain distinct. Version tokens glued directly to arbitrary letters, unusual Chinese/full-width version digits and complex version syntaxes may remain unknown.

Compare filename versions only with filename versions, and EXE resource versions only with EXE resource versions. A conflict in either comparable source prevents a duplicate result. Resource metadata may describe a bootstrapper rather than the installed product; it is evidence, not an authenticity guarantee. MSI properties, signatures, and internal archive manifests are not parsed.

Verified duplicate results for unversioned packages require matching sampled content. Known equal versions allow same canonical name with sizes within 10%, even if sampled bytes differ (a possible rebuild). Early hints are a separate unverified category: exact canonical name only, with no size/version/content claim. Known filename-version differences remain separate canonical names. Matching samples are not presented as byte-for-byte identity. There is no invented probability score.

Age uses the earliest available creation/modification timestamp plus three UTC calendar months, including end-of-month clamping. Only after that exact anniversary does `old_age=true`. This is approximate filesystem age, not a download-history database. Different-version age advisories are distinct from duplicate alarms and never compare version chronology lexicographically.

## Whitelist boundary and actions

Only explicit roots are watched. Overlapping roots are rejected so removing one root has unambiguous meaning. Resolve selected root paths once; verify descendants remain within the root and are not reparse points before inspecting, sampling or acting. No child-link traversal. The directory chooser grants access to the selected canonical local destination.

Explorer selection converts canonical filesystem paths to Shell display form and uses `SHParseDisplayName` + `SHOpenFolderAndSelectItems`. If selection fails, `ShellExecuteW` opens only the parent directory. No shell command, installer launch or command-line interpolation is used. Revalidate before opening. Removing either related root or deleting either related file drops queued alarms. The UI does not expose delete, cancel, install or execute buttons.

## Current operational limits

The first version requires the user process to be running. Downloads completed while it was stopped become baseline records on the next scan and do not create historical alerts. Local watchers are not a perfect download-completion API; paused writers with released handles can defeat readiness heuristics.

Random browser intermediate names do not disclose their target; those downloads need final-name events. A very fast download can complete before OS notifications or UI presentation. These limits cannot be fixed by a faster name-search algorithm alone. Large index scans are serialized on the worker, so commands and download notifications can still wait during startup/rebuild; early warning assumes the initial index has finished. Event queue: 1,024 inputs; pending paths and active download sessions: 4,096 each; UI mailbox: 256 outputs; alarm waiting queue: 128. Overflow recovery is silent indexing plus a visible error about potentially missed alarms. Root reconnection is manual via Rebuild. There is no idle periodic polling, scheduled cleanup or benchmark-backed claim of a particular Windows RAM/CPU figure.

Reference documentation:
- https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-readdirectorychangesw
- https://docs.rs/notify/8.2.0/notify/
- https://learn.microsoft.com/en-us/windows/win32/menurc/versioninfo-resource
- https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shopenfolderandselectitems
- https://docs.rs/embed-resource/3.0.12/embed_resource/
- https://github.com/notify-rs/notify/blob/main/notify/src/windows.rs
- https://chromium.googlesource.com/chromium/src/+/refs/heads/main/chrome/browser/download/download_target_determiner.cc

