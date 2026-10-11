/// Convert a canonical filesystem path into a Shell display path. Rust's
/// Windows canonicalize returns a verbatim path; that prefix belongs to file
/// APIs, not the Explorer namespace parser. Work on UTF-16 units so Chinese,
/// supplementary characters and even unpaired units are never silently lost.
pub fn display_path(units: &[u16]) -> Option<Vec<u16>> {
    if units.contains(&0) {
        return None;
    }
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    if !units.starts_with(&prefix) {
        return Some(units.to_vec());
    }
    let tail = &units[prefix.len()..];
    let unc: Vec<u16> = r"UNC\".encode_utf16().collect();
    if tail.len() >= unc.len()
        && tail[..unc.len()]
            .iter()
            .zip(&unc)
            .all(|(a, b)| (if (97..=122).contains(a) { *a - 32 } else { *a }) == *b)
    {
        let mut result = vec![92, 92];
        result.extend_from_slice(&tail[unc.len()..]);
        return Some(result);
    }
    if tail.len() >= 3
        && ((65..=90).contains(&tail[0]) || (97..=122).contains(&tail[0]))
        && tail[1] == 58
        && tail[2] == 92
    {
        return Some(tail.to_vec());
    }
    // Device/volume namespace paths cannot be converted by merely stripping
    // a prefix. The app authorizes only drive-letter roots.
    None
}
