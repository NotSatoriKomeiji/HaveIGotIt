use regex::Regex;
use std::sync::LazyLock;

static COPY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*(?:\([0-9]+\)|（[0-9]+）)$").unwrap());
// Only dotted versions or explicitly v-prefixed integers. Architecture numbers
// (x64, win32), dates and ordinary product numbers are deliberately retained.
static VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[-_\s])v?([0-9]+(?:\.[0-9]+){1,4}(?:[-_](?:alpha|beta|rc|pre)[0-9]*)?|v[0-9]+)(?:$|[-_\s])").unwrap()
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    pub canonical: String,
    pub identity: String,
    pub extension: String,
    pub version: Option<String>,
}

pub fn parse(filename: &str) -> Option<Name> {
    let lower = filename.to_lowercase();
    let ext = [
        "tar.gz",
        "tar.xz",
        "tar.bz2",
        "exe",
        "msi",
        "msix",
        "msixbundle",
        "zip",
        "rar",
        "7z",
        "iso",
        "cab",
    ]
    .into_iter()
    .find(|ext| lower.ends_with(&format!(".{ext}")))?;
    let mut stem = lower[..lower.len() - ext.len() - 1].trim().to_owned();
    // Browsers can append a second counter to an already suffixed download.
    while COPY.is_match(&stem) {
        stem = COPY.replace(&stem, "").trim().to_owned();
    }
    if stem.is_empty() {
        return None;
    }
    let canonical = format!("{stem}.{ext}");
    let mut identity = stem.clone();
    let version = VERSION.captures(&stem).and_then(|c| c.get(1)).map(|m| {
        let value = m.as_str().trim_start_matches('v').replace('_', "-");
        identity.replace_range(m.range(), "{version}");
        value
    });
    Some(Name {
        canonical,
        identity: format!("{identity}.{ext}"),
        extension: ext.into(),
        version,
    })
}
