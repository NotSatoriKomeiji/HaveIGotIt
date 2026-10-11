use crate::names::{self, Name};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Debug, Clone)]
pub struct Record {
    pub path: PathBuf,
    pub name: Name,
    pub size: u64,
    pub modified_ns: String,
    pub age_time: i64,
    pub pe_version: Option<String>,
    pub sample: Option<String>,
}

pub fn is_link(path: &Path) -> bool {
    // Let traversal report unreadable entries; silently treating every metadata
    // error as a link would incorrectly mark a partial scan as complete.
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

pub fn local_root(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let letter = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
                _ => return false,
            },
            _ => return false,
        };
        let root = [letter as u16, 58, 92, 0];
        use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
        use windows_sys::Win32::System::WindowsProgramming::{
            DRIVE_NO_ROOT_DIR, DRIVE_REMOTE, DRIVE_UNKNOWN,
        };
        let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
        ![DRIVE_REMOTE, DRIVE_UNKNOWN, DRIVE_NO_ROOT_DIR].contains(&kind)
    }
    #[cfg(not(windows))]
    {
        path.is_absolute()
    }
}

pub fn permitted(path: &Path, root: &Path) -> bool {
    within_root_without_links(path, root)
        && path
            .canonicalize()
            .is_ok_and(|canonical| canonical.starts_with(root))
}

/// Also works for missing leaves: stop at a child reparse point before looking
/// up metadata beneath it. Maintenance must not follow a newly replaced parent.
pub fn within_root_without_links(path: &Path, root: &Path) -> bool {
    if !path.starts_with(root) || path == root {
        return false;
    }
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut cursor = root.to_owned();
    for part in relative.components() {
        if !matches!(part, std::path::Component::Normal(_)) {
            return false;
        }
        cursor.push(part);
        if is_link(&cursor) {
            return false;
        }
    }
    true
}

pub fn open_read(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1); // FILE_SHARE_READ: do not read while a writer has it open.
    }
    options.open(path)
}

pub fn inspect(path: &Path) -> io::Result<Option<Record>> {
    let mut record = inspect_metadata(path)?;
    if let Some(value) = &mut record {
        value.pe_version = crate::version::read(path);
    }
    Ok(record)
}

/// Metadata only: early hints never read incomplete executables or hashes.
pub fn inspect_metadata(path: &Path) -> io::Result<Option<Record>> {
    let Some(name) = path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(names::parse)
    else {
        return Ok(None);
    };
    let meta = fs::metadata(path)?;
    if !meta.is_file() || meta.len() == 0 {
        return Ok(None);
    }
    let modified = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let created = meta
        .created()
        .ok()
        .and_then(|d| d.duration_since(UNIX_EPOCH).ok());
    let age_time =
        created.map_or(modified.as_secs(), |d| d.as_secs().min(modified.as_secs())) as i64;
    Ok(Some(Record {
        path: path.into(),
        name,
        size: meta.len(),
        modified_ns: modified.as_nanos().to_string(),
        age_time,
        pe_version: None,
        sample: None,
    }))
}

pub fn fingerprint(record: &Record) -> (u64, String) {
    (record.size, record.modified_ns.clone())
}

/// Domain-separated sampled hash of size + first/centre/last 64 KiB. A matching
/// sample is evidence of a possible duplicate, never proof of byte identity.
pub fn sample(file: &mut File) -> io::Result<String> {
    const BLOCK: u64 = 64 * 1024;
    let before = file.metadata()?;
    let size = before.len();
    let modified = before.modified()?;
    let mut hash = Sha256::new();
    hash.update(b"HaveIGotIt/sample/v1\0");
    hash.update(size.to_le_bytes());
    let mut offsets = vec![
        0,
        size.saturating_sub(BLOCK) / 2,
        size.saturating_sub(BLOCK),
    ];
    offsets.sort_unstable();
    offsets.dedup();
    let mut buffer = vec![0; BLOCK as usize];
    for offset in offsets {
        file.seek(SeekFrom::Start(offset))?;
        let count = (size - offset).min(BLOCK) as usize;
        file.read_exact(&mut buffer[..count])?;
        hash.update(offset.to_le_bytes());
        hash.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if after.len() != size || after.modified()? != modified {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "file changed during sampling",
        ));
    }
    Ok(format!("{:x}", hash.finalize()))
}
