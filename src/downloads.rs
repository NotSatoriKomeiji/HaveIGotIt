//! Temporary download names are hints, never proof of size/content/version.
use crate::{
    db::Db,
    files,
    matching::{self, Alert, Reason},
    names,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

pub fn alert_valid(alert: &Alert, folders: &[crate::db::Folder]) -> bool {
    let permitted =
        |path: &Path| folders.iter().any(|f| files::permitted(path, &f.path)) && path.is_file();
    if !permitted(&alert.old_file.path) {
        return false;
    }
    // Recheck immediately before presentation. A cached/queued alarm must not
    // survive deletion, replacement by a directory or a changed old package.
    let old_unchanged = files::inspect_metadata(&alert.old_file.path).is_ok_and(|old| {
        old.is_some_and(|old| files::fingerprint(&old) == files::fingerprint(&alert.old_file))
    });
    old_unchanged
        && (permitted(&alert.new_file.path)
            || (alert.reason == Reason::Downloading
                && target(&alert.new_file.path).is_some_and(|p| permitted(&p))))
}

/// Event-driven withdrawals only: never append an ignored alarm.
pub fn prune_alerts(
    current: &mut Option<Alert>,
    queued: &mut VecDeque<Alert>,
    folders: &[crate::db::Folder],
) -> bool {
    queued.retain(|alert| alert_valid(alert, folders));
    let withdrawn = current
        .as_ref()
        .is_some_and(|alert| !alert_valid(alert, folders));
    if withdrawn {
        *current = None;
    }
    withdrawn
}

/// Returns true when a visible hint was withdrawn. Empty/dismissed queues stay
/// empty, even when verification succeeds; IDs distinguish reused file paths.
pub fn finish_alerts(
    current: &mut Option<Alert>,
    queued: &mut VecDeque<Alert>,
    id: u64,
    replacement: Option<&Alert>,
) -> bool {
    let visible = current.as_ref().is_some_and(|a| a.download_id == Some(id));
    if let Some(replacement) = replacement {
        for existing in current.iter_mut().chain(queued.iter_mut()) {
            if existing.download_id == Some(id) {
                *existing = replacement.clone();
            }
        }
        false
    } else {
        queued.retain(|a| a.download_id != Some(id));
        if visible {
            *current = None;
        }
        visible
    }
}

/// Only strip known browser suffixes when the remaining name is supported.
/// Random `Unconfirmed 123.crdownload` names cannot reveal the intended file.
pub fn target(path: &Path) -> Option<PathBuf> {
    let filename = path.file_name()?.to_str()?;
    let lower = filename.to_ascii_lowercase();
    let suffix = [".crdownload", ".part"]
        .into_iter()
        .find(|s| lower.ends_with(s))?;
    let final_name = &filename[..filename.len() - suffix.len()];
    names::parse(final_name)?;
    Some(path.with_file_name(final_name))
}

/// An indexed exact-name lookup; never read or lock the incomplete download.
pub fn preview(db: &Db, temporary: &Path, final_path: &Path) -> Result<Option<Alert>, String> {
    let name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(names::parse);
    let Some(name) = name else { return Ok(None) };
    let folders = db.folders().map_err(|e| e.to_string())?;
    if !folders.iter().any(|f| files::permitted(temporary, &f.path)) {
        return Ok(None);
    }
    if !std::fs::metadata(temporary).is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    for cached in db
        .named_candidates(&name, final_path)
        .map_err(|e| e.to_string())?
    {
        if !folders
            .iter()
            .any(|f| files::permitted(&cached.path, &f.path))
        {
            continue;
        }
        let Ok(Some(mut old)) = files::inspect_metadata(&cached.path) else {
            continue;
        };
        // Preserve resource evidence only if the old file has not changed.
        if files::fingerprint(&old) == files::fingerprint(&cached) {
            old.pe_version = cached.pe_version;
        }
        let new = files::Record {
            path: temporary.to_owned(),
            name: name.clone(),
            size: 0,
            modified_ns: String::new(),
            age_time: 0,
            pe_version: None,
            sample: None,
        };
        return Ok(Some(Alert {
            new_file: new,
            old_age: matching::older_than_three_months(old.age_time, chrono::Utc::now()),
            old_file: old,
            reason: Reason::Downloading,
            download_id: None,
        }));
    }
    Ok(None)
}
