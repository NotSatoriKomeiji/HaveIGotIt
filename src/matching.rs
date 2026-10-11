use crate::files::Record;
use chrono::{DateTime, Months, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Downloading,
    SampleMatch,
    SameVersion,
    OlderVersion,
}

#[derive(Debug, Clone)]
pub struct Alert {
    /// Identifies a temporary-file download across renames and UI dismissal.
    pub download_id: Option<u64>,
    pub new_file: Record,
    pub old_file: Record,
    pub reason: Reason,
    pub old_age: bool,
}

/// Relative to the larger file, inclusive at 10%, without float roundoff.
pub fn size_close(a: u64, b: u64) -> bool {
    a > 0 && b > 0 && u128::from(a.abs_diff(b)) * 10 <= u128::from(a.max(b))
}

/// Strictly more than three calendar months. The first known file date is
/// metadata evidence, not a claim that we know when the browser downloaded it.
pub fn older_than_three_months(timestamp: i64, now: DateTime<Utc>) -> bool {
    DateTime::from_timestamp(timestamp, 0)
        .and_then(|date| date.checked_add_months(Months::new(3)))
        .is_some_and(|anniversary| now > anniversary)
}

pub fn classify(
    new: &Record,
    old: &Record,
    samples_equal: bool,
    now: DateTime<Utc>,
) -> Option<Alert> {
    if new.path == old.path || new.name.extension != old.name.extension {
        return None;
    }
    let old_age = older_than_three_months(old.age_time, now);
    // Version provenance is kept separate; do not compare filename versions to
    // installer resource versions, which often describe the bootstrapper.
    let conflict = new
        .name
        .version
        .as_ref()
        .zip(old.name.version.as_ref())
        .is_some_and(|(a, b)| a != b)
        || new
            .pe_version
            .as_ref()
            .zip(old.pe_version.as_ref())
            .is_some_and(|(a, b)| a != b);
    if conflict {
        // A different version is never called a duplicate. An age advisory is a
        // separate category, restricted to the same filename identity (including
        // architecture/channel/package format). Unknown chronology is not called
        // an upgrade or downgrade.
        if old_age && new.name.identity == old.name.identity {
            return Some(Alert {
                download_id: None,
                new_file: new.clone(),
                old_file: old.clone(),
                reason: Reason::OlderVersion,
                old_age,
            });
        }
        return None;
    }
    if new.name.canonical != old.name.canonical || !size_close(new.size, old.size) {
        return None;
    }
    let same_version = new
        .name
        .version
        .as_ref()
        .zip(old.name.version.as_ref())
        .is_some_and(|(a, b)| a == b)
        || new
            .pe_version
            .as_ref()
            .zip(old.pe_version.as_ref())
            .is_some_and(|(a, b)| a == b);
    let reason = if samples_equal {
        Reason::SampleMatch
    } else if same_version {
        Reason::SameVersion
    } else {
        return None;
    };
    Some(Alert {
        download_id: None,
        new_file: new.clone(),
        old_file: old.clone(),
        reason,
        old_age,
    })
}
