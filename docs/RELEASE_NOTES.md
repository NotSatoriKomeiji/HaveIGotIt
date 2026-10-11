HaveIGotIt? v0.1.0 — first trial release / 首版试用

- Native Windows settings window and tray; no WebView.
- Explicit folder whitelist with add/remove and local SQLite indexing.
- Event-driven early named-download hints, final verification, incremental matching and index maintenance.
- Browser `(1)` / `（2）` filename-counter support.
- Known version conflicts prevent duplicate warnings.
- Sampled-content evidence and at-most-10% size tolerance for known matching versions.
- Three-calendar-month age note, with distinct advisory for different known versions.
- Exactly two alarm actions: Open Location / Ignore.
- Ignore applies only to this download; no deletion, cancellation or installer execution.
- English/Chinese defaults in separate ZIPs; language switch in both.
- DPI-aware layout, taskbar work-area positioning and Explorer-native file selection.

See `docs/VALIDATION.md` for completed checks and outstanding Windows acceptance. This initial cross-compiled build has not been run on a Windows machine. No measured resource-use claim is made.


Explorer-location hotfix: normalize verbatim paths before handing them to Explorer; preserve Chinese/Unicode filenames; balance COM initialization; open the parent directory if selection is unavailable; include native error codes if both operations fail. Four additional conversion tests passed (24 total core/regression tests).

Early-warning/latency fix: named `.crdownload` / `.part` files can warn while still empty, explicitly stating that final size/version/content are unverified. Final renames no longer wait a fixed four seconds; ordinary final-name writes use two 150 ms quiet checks. Verification updates/withdraws the same alarm and never resurrects Ignore; new downloads, including reuse of the same destination, receive new session IDs. Random temporary names still need final-name events. Indexed prefix removal avoids full-table deletion scans; unmatched downloads skip sampled reads. Five more tests pass (29 total), with synthetic Linux timing recorded in `docs/VALIDATION.md`; Windows visible-popup timing remains unmeasured.

Final UI/index update: remove the two-buttons/your-decision sentence and close-to-tray footer. Coalesce indexed-file/directory deletion events using one second of quiet, then run one metadata-only verification sweep in bounded 64-record pages with an 8 ms soft budget. No recurring timer or temporary-file-triggered sweep. Missing rows are removed and stale hash/version caches invalidated; unreadable/unavailable paths retain their records. Alarm presentation rechecks live old-file metadata and withdraws missing/changed candidates. Cache cleanup modifies only internal data, never user files. Seven more tests pass (36 total); Windows UI runtime/resource measurements are still outstanding.
