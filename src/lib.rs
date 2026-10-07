//
// unicode-fetch .................. src/lib.rs
// copyright (c) 2026 malakai smith (@tenault)
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0.
//

// ~~~~~~~~~~~~~~~~~~~~~~
// [[    ENVIRONMENT   ]]
// ~~~~~~~~~~~~~~~~~~~~~~

#![forbid(unsafe_code)]

use std::{
    env, error, fmt, fs, io,
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};


// ~~~~~~~~~~~~~~~~~~
// [[    SYMBOLS   ]]
// ~~~~~~~~~~~~~~~~~~

const UNICODE_URL: &str = "https://unicode.org/Public";

#[cfg(feature = "https")]
const MAX_SIZE: u64 = 50 * 1024 * 1024; // 50MB safety limit

pub type FetchResult<T> = Result<T, FetchError>;


// ~~~~~~~~~~~~~~~~~~~~~~~~~~~
// [[    UNICODE FETCHER    ]]
// ~~~~~~~~~~~~~~~~~~~~~~~~~~~

// ~~~~~ FETCHER ~~~~~

#[derive(Clone, Debug)]
pub struct Fetcher {
    root: PathBuf,
    version: String,
    cache_lifetime: Option<Duration>,

    #[cfg(feature = "checksum")]
    expected_hash: Option<String>,
}

impl Fetcher {

    // ,,,,,,,,,,,,,,,,,,,,,
    // [    constructor    ]
    // '''''''''''''''''''''

    /// Create a new [`Fetcher`] with root set at the given path.
    pub fn with_root(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
            version: "latest".to_string(),
            cache_lifetime: None,

            #[cfg(feature = "checksum")]
            expected_hash: None,
        }
    }

    // ,,,,,,,,,,,,,,,
    // [    fetch    ]
    // '''''''''''''''

    /// Fetch a data file from unicode.org.
    ///
    /// ## Arguments
    ///
    /// - `file`: The target filename, optionally prefixed with a subdirectory.
    ///           Examples: `"GraphemeBreakTest.txt"`, `"emoji/emoji-data.txt"`,
    ///                     `"auxiliary/GraphemeBreakTest.txt"`.
    /// - `dir`:  The target directory inside the manifest root where the fetched file will live.
    ///           If empty, files will be stored at the top level of the manifest root.
    ///
    /// ## Returns
    ///
    /// The target file's contents as a `String` if successful.
    ///
    /// ## Quirks
    ///
    /// If the target file contains a subdirectory (e.g. `"auxiliary/WordBreakTest.txt"`), then that
    /// subdirectory will be present in the fetch url, but _not_ in the output directory. Therefore,
    /// running something like `fetch("extracted/DerivedLineBreak.txt", "data/unicode");` will store
    /// the resulting file at `data/unicode/DerivedLineBreak.txt`.
    ///
    /// ## Errors
    ///
    /// See [`FetchError`].
    pub fn fetch(&self, file: &str, dir: &str) -> FetchResult<String> {

        // ..... prevent traversal .....

        let root = safe_root(&self.root)?;
        let dir  = safe_path(dir)?;
        let file = safe_path(file)?;

        // ..... check for existing target .....

        let (subdir, name) = get_unicode_path(&file);
        let dest = root.join(&dir).join(&name);

        if self.file_exists(&dest) { return read_file(&dest); }

        // ..... download target file .....

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| FetchError::IO {
                source: e,
                path: parent.to_path_buf(),
            })?;
        }

        let tmp = dest.with_extension("part");
        let url = build_url(&self.version, &subdir, &name)?;

        download_file(&url, &tmp)?;

        // ..... check integrity (optional) .....

        #[cfg(feature = "checksum")]
        if let Some(ref expected) = self.expected_hash {
            let computed = get_hash(&tmp)?;

            if !expected.eq_ignore_ascii_case(&computed) {
                let _ = fs::remove_file(&tmp);
                return Err(FetchError::HashMismatch {
                    file: name.to_string(),
                    expected: expected.clone(),
                    computed,
                });
            }
        }

        // ..... save + read .....

        fs::rename(&tmp, &dest).map_err(|e| FetchError::IO {
            source: e,
            path: dest.to_path_buf(),
        })?;

        read_file(&dest)
    }

    // ,,,,,,,,,,,,,,,,,,,
    // [    accessors    ]
    // '''''''''''''''''''

    /// Set the unicode version to fetch from.
    ///
    /// Can be `latest`, or a dot-separated version string like `18.0.0`. When [`Fetcher::fetch`] is
    /// called, this string is injected directly into the target url. Validation of this string only
    /// happens as part of that process.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    /// Set the maximum lifetime of a target file when fetching.
    ///
    /// When [`Fetcher::fetch`] is called, the age of the target file is compared against this value
    /// and re-downloaded only if it exceeds it. If set to `None`, files are cached indefinitely.
    pub fn with_cache_lifetime(mut self, duration: Duration) -> Self {
        self.cache_lifetime = Some(duration);
        self
    }

    /// Set the expected SHA-2 hash for the target file.
    ///
    /// Requires the `checksum` feature. When [`Fetcher::fetch`] is called, the resulting SHA-2 hash
    /// of the target file is checked against this string, with a [`FetchError::HashMismatch`] being
    /// thrown if they differ. Validation of this string's hex format/length only happens as part of
    /// that process. If set to `None`, this check is skipped entirely.
    #[cfg(feature = "checksum")]
    pub fn with_expected_hash(mut self, hash: impl Into<String>) -> Self {
        self.expected_hash = Some(hash.into());
        self
    }

    // ,,,,,,,,,,,,,,,,,
    // [    utility    ]
    // '''''''''''''''''

    /// Check if the target file already exists, and is under the cache lifetime (if set).
    fn file_exists(&self, path: &Path) -> bool {
        if !path.exists() { return false; }

        if let Some(duration) = self.cache_lifetime {
            let metadata = match fs::metadata(path) {
                Ok(m)  => m,
                Err(_) => return false,
            };

            let modified = match metadata.modified() {
                Ok(t)  => t,
                Err(_) => return false,
            };

            let elapsed = match modified.elapsed() {
                Ok(d)  => d,
                Err(_) => return false,
            };

            if elapsed > duration { return false; }
        }

        true
    }
}

// ~~~~~ CONVENIENCE ~~~~~

/// Fetch a unicode data `file` and store it at `dir`.
///
/// For convenience, this resolves the root directory at runtime from the current cargo environment,
/// which is only available whenever cargo is driving (via `cargo run`/`cargo test`). Binaries which
/// run detached outside cargo should prefer the compile-time macro [`fetch!`] instead.
pub fn fetch(file: &str, dir: &str) -> FetchResult<String> {
    let root = env::var_os("CARGO_MANIFEST_DIR").ok_or(FetchError::MissingRoot)?;
    fetch_with_root(root, file, dir)
}

/// Fetch a unicode data `file` and store it at `dir` inside `root`.
///
/// <div class="warning">
///
/// This enables clients to explicitly set the root directory outside the current cargo environment,
/// which can sometimes be useful. However, since this crate doesn't blacklist OS-critical pathnames
/// (like `/etc/passwd`), this runs the risk of overwriting vital files, and caution is advised when
/// using it.
///
/// </div>
///
/// Internally, both [`fetch()`] and [`fetch!`] quietly target this, resolving the root directory at
/// runtime or compile-time, respectively.
pub fn fetch_with_root(root: impl AsRef<Path>, file: &str, dir: &str) -> FetchResult<String> {
    Fetcher::with_root(root).fetch(file, dir)
}

/// Compile-time form of [`fetch()`].
///
/// This can (theoretically) be useful for detached binaries that run outside the cargo environment,
/// yet still want fetched files to live inside the root cargo manifest directory. Certain pipelines
/// sometimes operate this way.
#[macro_export]
macro_rules! fetch {
    ($file:expr, $dir:expr) => {
        $crate::fetch_with_root(env!("CARGO_MANIFEST_DIR"), $file, $dir)
    };
}


// ~~~~~~~~~~~~~~~~~~~
// [[    UTILITY    ]]
// ~~~~~~~~~~~~~~~~~~~

// ~~~~~ FILES ~~~~~

/// Read file contents as a string.
fn read_file(path: &Path) -> FetchResult<String> {
    fs::read_to_string(path).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            FetchError::UTF8 { source: e, path: path.to_path_buf() }
        } else {
            FetchError::IO { source: e, path: path.to_path_buf() }
        }
    })
}

/// Download a target file at `url` and store it at `dest`.
fn download_file(url: &str, dest: &Path) -> FetchResult<()> {
    #[cfg(feature = "https")]
    {
        if ureq(url, dest)? { return Ok(()); }
    }

    if curl(url, dest) || wget(url, dest) { return Ok(()); }

    Err(FetchError::MissingDownloader)
}

/// Compute the SHA-256 of a target file.
#[cfg(feature = "checksum")]
fn get_hash(path: &Path) -> FetchResult<String> {
    use sha2::{Digest, Sha256};

    let data = fs::read(path).map_err(|e| FetchError::IO {
        source: e,
        path: path.to_path_buf(),
    })?;

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let result = hasher.finalize();

    Ok(format!("{:?}", result))
}

// ~~~~~ INTERNET ~~~~~

/// Download the target file at `url` via ureq, and store it at `dest`.
#[cfg(feature = "https")]
fn ureq(url: &str, dest: &Path) -> FetchResult<bool> {

    // ..... import writer .....

    use io::{copy, BufWriter, Read, Write};

    // ..... build agent + fetch url .....

    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build();

    let agent: ureq::Agent = config.into();

    let response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(status) => FetchError::HTTP {
            url: url.to_owned(),
            status,
        },
        e => FetchError::Download {
            url: url.to_owned(),
            reason: e.to_string(),
        },
    })?;

    // ..... pre-reject big files .....

    if let Some(length) = response.headers().get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok()) {
        if length > MAX_SIZE {
            return Err(FetchError::Download {
                url: url.to_owned(),
                reason: format!("file size exceeds safety limit ({length} > {MAX_SIZE})"),
            });
        }
    }

    // ..... copy to dest .....

    let file = fs::File::create(dest).map_err(|e| FetchError::IO {
        source: e,
        path: dest.to_path_buf(),
    })?;

    let mut writer = BufWriter::new(file);

    let bytes = copy(&mut response.into_body().into_reader().take(MAX_SIZE + 1), &mut writer)
        .map_err(|_| FetchError::Download {
            url: url.to_owned(),
            reason: "failed to read response body".to_owned(),
        })?;

    writer.flush().map_err(|e| FetchError::IO {
        source: e,
        path: dest.to_path_buf(),
    })?;

    // ..... post-write size check .....

    if bytes > MAX_SIZE {
        let _ = fs::remove_file(dest);
        return Err(FetchError::Download {
            url: url.to_owned(),
            reason: format!("file size exceeds safety limit ({bytes} > {MAX_SIZE})"),
        });
    }

    Ok(true)
}

/// Download the target file at `url` via curl, and store it at `dest`.
fn curl(url: &str, dest: &Path) -> bool {
    Command::new("curl")
        .args(["-fsSL", "--connect-timeout", "10", "-o"])
        .arg(dest)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Download the target file at `url` via wget, and store it at `dest`.
fn wget(url: &str, dest: &Path) -> bool {
    Command::new("wget")
        .args(["-q", "--tries=3", "--timeout=10", "-O"])
        .arg(dest)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ~~~~~ PATHS ~~~~~

/// Validate the root path has no traversal components.
fn safe_root(root: &Path) -> FetchResult<PathBuf> {
    for component in root.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(FetchError::UnsafePath(root.to_string_lossy().into_owned()));
            },
            Component::CurDir | Component::Normal(_) => { /* path is safe */ },
        }
    }

    Ok(root.to_path_buf())
}

/// Validate that a given path has no traversal components.
fn safe_path(path: &str) -> FetchResult<String> {
    let test = path.replace('\\', "/");

    if test.is_empty() || test.starts_with('/') || test.contains(":/") {
        return Err(FetchError::UnsafePath(path.to_string()));
    }

    let clone = Path::new(&test);
    for component in clone.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(FetchError::UnsafePath(path.to_string()));
            },
            Component::CurDir | Component::Normal(_) => { /* path is safe */ },
        }
    }

    Ok(test)
}

/// Validate that a given version is in the correct format.
fn safe_version(version: &str) -> FetchResult<String> {
    if version == "latest" { return Ok("UCD/latest".to_string()); }

    let parts: Vec<&str> = version.split('.').collect();

    if parts.len() != 3 || !parts.iter().all(|p| p.parse::<u32>().is_ok()) {
        return Err(FetchError::InvalidVersion(version.to_string()));
    }

    Ok(version.to_string())
}

// ~~~~~ UNICODE ~~~~~

/// Get the unicode url of the target file.
fn build_url(version: &str, subdir: &str, file: &str) -> FetchResult<String> {
    let version = safe_version(version)?;
    let subdir = if !subdir.is_empty() { format!("ucd/{subdir}") } else { "ucd".to_string() };

    Ok(format!("{}/{}/{}/{}", UNICODE_URL, version, subdir, file))
}

/// Get the subdirectory of a given unicode data file, if it exists.
fn get_unicode_path(file: &str) -> (&str, &str) {
    if let Some(parts) = file.split_once('/') { return (parts.0, parts.1); }

    return match file {
        "GraphemeBreakProperty.txt"
            | "GraphemeBreakTest.txt"
            | "LineBreakTest.txt"
            | "SentenceBreakProperty"
            | "SentenceBreakTest.txt"
            | "WordBreakProperty"
            | "WordBreakTest.txt" => ("auxiliary", file),
        "DerivedBidiClass.txt"
            | "DerivedLineBreak.txt" => ("extracted", file),
        _ if file.starts_with("emoji-") => ("emoji", file),
        _ => ("", file),
    }
}


// ~~~~~~~~~~~~~~~~~~
// [[    ERRORS    ]]
// ~~~~~~~~~~~~~~~~~~

#[derive(Debug)]
pub enum FetchError {
    MissingRoot,

    UnsafePath(String),

    InvalidVersion(String),

    #[cfg(feature = "checksum")]
    InvalidHash(String),

    IO {
        source: io::Error,
        path: PathBuf,
    },

    MissingDownloader,

    #[cfg(feature = "https")]
    HTTP { url: String, status: u16 },

    #[cfg(feature = "https")]
    Download { url: String, reason: String },

    #[cfg(feature = "checksum")]
    HashMismatch { file: String, expected: String, computed: String },

    UTF8 {
        source: io::Error,
        path: PathBuf,
    },
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::MissingRoot => {
                write!(f, "manifest not set: run via cargo, or use 'fetch!'/'Fetcher::with_root()'")
            },
            FetchError::UnsafePath(path) => {
                write!(f, "'{path}' is not permitted: only plain components below root are allowed")
            },
            FetchError::InvalidVersion(v) => {
                write!(f, "version '{v}' is not valid: expected dot-separated digits like '18.0.0'")
            },
            #[cfg(feature = "checksum")]
            FetchError::InvalidHash(hash) => {
                write!(f, "hash '{hash}' is not valid: expected 64 hex chars")
            },
            FetchError::IO { source, path } => {
                write!(f, "io failure at '{}': {source}", path.display())
            },
            FetchError::MissingDownloader => {
                write!(f, "missing downloader: build with '--features https' or install curl/wget")
            },
            #[cfg(feature = "https")]
            FetchError::HTTP { url, status } => {
                write!(f, "http status '{status}' for '{url}'")
            },
            #[cfg(feature = "https")]
            FetchError::Download { url, reason } => {
                write!(f, "could not fetch '{url}': {reason}")
            },
            #[cfg(feature = "checksum")]
            FetchError::HashMismatch { file, expected, computed } => {
                write!(f, "hash mismatch for '{file}': '{computed}' does not match '{expected}'")
            },
            FetchError::UTF8 { source, path } => {
                write!(f, "'{}' is not valid utf-8: {source}", path.display())
            },
        }
    }
}

impl error::Error for FetchError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            FetchError::IO { source, .. } | FetchError::UTF8 { source, .. } => { Some(source) },
            _ => None,
        }
    }
}
