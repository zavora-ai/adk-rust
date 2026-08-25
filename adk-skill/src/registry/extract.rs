//! Safe extraction of Skill Registry zip payloads.
//!
//! The Skill Registry validates every uploaded archive server-side. This
//! module mirrors those rules as defense-in-depth, so a corrupt or hostile
//! payload is rejected locally with a typed [`SkillError`] before any bytes
//! reach the filesystem:
//!
//! | Rule | Limit | Error |
//! |------|-------|-------|
//! | Archive size | ≤ [`MAX_ARCHIVE_BYTES`] | [`SkillError::ArchiveTooLarge`] |
//! | Entry count | ≤ [`MAX_ENTRIES`] | [`SkillError::ArchiveTooManyEntries`] |
//! | `..` components | none | [`SkillError::ArchivePathTraversal`] |
//! | Absolute paths (`/`, `\`) | none | [`SkillError::ArchiveAbsolutePath`] |
//! | Symlinks | none | [`SkillError::ArchiveSymlink`] |
//! | Duplicate names | none | [`SkillError::ArchiveDuplicateEntry`] |
//! | Uncompressed total | ≤ [`MAX_UNCOMPRESSED_BYTES`] | [`SkillError::ArchiveUncompressedTooLarge`] |
//! | Compression ratio | ≤ [`MAX_COMPRESSION_RATIO`] | [`SkillError::ArchiveCompressionRatio`] |
//! | Directory depth | ≤ [`MAX_DIRECTORY_DEPTH`] | [`SkillError::ArchiveDepthExceeded`] |
//!
//! Validation runs in two phases: the central directory is checked first
//! (names, symlinks, duplicates, declared sizes, ratios) without
//! decompressing anything, then entry contents are read with the declared
//! size enforced as a hard read bound.

use crate::error::{SkillError, SkillResult};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use zip::ZipArchive;

/// Maximum accepted archive size in bytes (10 MB).
pub const MAX_ARCHIVE_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum number of entries in an archive.
pub const MAX_ENTRIES: usize = 10_000;
/// Maximum declared uncompressed total in bytes (500 MB).
pub const MAX_UNCOMPRESSED_BYTES: u64 = 500 * 1024 * 1024;
/// Maximum per-entry compression ratio (uncompressed / compressed).
pub const MAX_COMPRESSION_RATIO: u64 = 100;
/// Maximum directory nesting depth for any entry.
pub const MAX_DIRECTORY_DEPTH: usize = 8;

/// Safely extracts a skill zip archive into an in-memory file map.
///
/// Keys are the entry paths exactly as stored in the archive (always
/// `/`-separated after validation); values are the decompressed file
/// contents. Directory entries are validated but not materialized.
///
/// # Example
///
/// ```
/// use adk_skill::registry::extract_skill_archive;
/// use std::io::Write;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
/// writer.start_file("SKILL.md", zip::write::SimpleFileOptions::default())?;
/// writer.write_all(b"---\nname: demo\ndescription: demo skill\n---\nBody")?;
/// let bytes = writer.finish()?.into_inner();
///
/// let files = extract_skill_archive(&bytes)?;
/// assert!(files.contains_key("SKILL.md"));
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns a distinct [`SkillError`] variant for each violated rule listed
/// in the module documentation, or [`SkillError::ArchiveFormat`] when the
/// bytes are not a well-formed zip archive.
pub fn extract_skill_archive(bytes: &[u8]) -> SkillResult<BTreeMap<String, Vec<u8>>> {
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(SkillError::ArchiveTooLarge {
            size: bytes.len() as u64,
            limit: MAX_ARCHIVE_BYTES,
        });
    }

    // The zip reader indexes entries by name and silently collapses exact
    // duplicates (last entry wins), so the entry-count and duplicate rules
    // are checked against the raw central directory instead.
    let names = central_directory_names(bytes)?;
    if names.len() > MAX_ENTRIES {
        return Err(SkillError::ArchiveTooManyEntries { count: names.len(), limit: MAX_ENTRIES });
    }
    let mut seen = std::collections::BTreeSet::new();
    for name in &names {
        // `a/` and `a` also collide: extraction to a directory could not
        // materialize both.
        let normalized = name.trim_end_matches(['/', '\\']);
        if !seen.insert(normalized) {
            return Err(SkillError::ArchiveDuplicateEntry { name: name.clone() });
        }
    }

    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(format_error)?;

    // Phase 1: central-directory validation, no decompression.
    let mut declared_total: u64 = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(format_error)?;
        let name = entry.name().to_string();
        validate_entry_name(&name)?;
        if entry.is_symlink() {
            return Err(SkillError::ArchiveSymlink { name });
        }
        if entry.is_dir() {
            continue;
        }
        let declared = entry.size();
        declared_total = declared_total.saturating_add(declared);
        if declared_total > MAX_UNCOMPRESSED_BYTES {
            return Err(SkillError::ArchiveUncompressedTooLarge {
                total: declared_total,
                limit: MAX_UNCOMPRESSED_BYTES,
            });
        }
        let compressed = entry.compressed_size();
        if compressed > 0 && declared > compressed.saturating_mul(MAX_COMPRESSION_RATIO) {
            return Err(SkillError::ArchiveCompressionRatio {
                name,
                ratio: declared / compressed,
                limit: MAX_COMPRESSION_RATIO,
            });
        }
    }

    // Phase 2: bounded content reads.
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(format_error)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let declared = entry.size();
        let mut contents = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
        let read = std::io::copy(&mut (&mut entry).take(declared + 1), &mut contents)
            .map_err(|error| SkillError::ArchiveFormat { message: error.to_string() })?;
        if read != declared {
            return Err(SkillError::ArchiveFormat {
                message: format!(
                    "entry `{name}` decompressed to {read} bytes but declared {declared}"
                ),
            });
        }
        files.insert(name, contents);
    }
    Ok(files)
}

/// Safely extracts a skill zip archive into a caller-provided directory.
///
/// Runs the same validation as [`extract_skill_archive`], then writes each
/// file under `dir`, creating parent directories as needed. Returns the
/// written paths in sorted order. Intended for future `pull`-style CLI
/// tooling; extraction is fully validated in memory before anything touches
/// the filesystem.
///
/// # Errors
///
/// Returns the same errors as [`extract_skill_archive`], plus
/// [`SkillError::Io`] when a file or directory cannot be written.
pub fn extract_skill_archive_to_dir(bytes: &[u8], dir: &Path) -> SkillResult<Vec<PathBuf>> {
    let files = extract_skill_archive(bytes)?;
    write_files_to_dir(&files, dir)
}

/// Writes an already-validated file map under `dir`.
pub(crate) fn write_files_to_dir(
    files: &BTreeMap<String, Vec<u8>>,
    dir: &Path,
) -> SkillResult<Vec<PathBuf>> {
    let mut written = Vec::with_capacity(files.len());
    for (name, contents) in files {
        // Validated names are relative with no `..` components, so the join
        // cannot escape `dir`.
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
        written.push(path);
    }
    Ok(written)
}

fn format_error(error: zip::result::ZipError) -> SkillError {
    SkillError::ArchiveFormat { message: error.to_string() }
}

/// Reads every entry name from the raw central directory.
///
/// The zip reader indexes entries by name, so exact duplicates and the true
/// entry count are only observable in the raw record stream.
fn central_directory_names(bytes: &[u8]) -> SkillResult<Vec<String>> {
    const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const CENTRAL_HEADER_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    const EOCD_LEN: usize = 22;
    const CENTRAL_HEADER_LEN: usize = 46;
    const MAX_COMMENT_LEN: usize = u16::MAX as usize;

    let read_u16 = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    let read_u32 = |offset: usize| {
        u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
            as usize
    };

    if bytes.len() < EOCD_LEN {
        return Err(SkillError::ArchiveFormat {
            message: "missing end-of-central-directory record".to_string(),
        });
    }
    let search_start = bytes.len().saturating_sub(EOCD_LEN + MAX_COMMENT_LEN);
    let eocd = (search_start..=bytes.len() - EOCD_LEN)
        .rev()
        .find(|&offset| bytes[offset..offset + 4] == EOCD_SIGNATURE)
        .ok_or_else(|| SkillError::ArchiveFormat {
            message: "missing end-of-central-directory record".to_string(),
        })?;

    let total_entries = read_u16(eocd + 10);
    let directory_size = read_u32(eocd + 12);
    let directory_offset = read_u32(eocd + 16);
    if total_entries == u16::MAX as usize
        || directory_size == u32::MAX as usize
        || directory_offset == u32::MAX as usize
    {
        // Zip64 markers. Archives within MAX_ARCHIVE_BYTES never need zip64,
        // so anything carrying the markers is rejected outright.
        return Err(SkillError::ArchiveFormat {
            message: "zip64 archives are not supported; repackage without zip64".to_string(),
        });
    }
    let directory_end = directory_offset
        .checked_add(directory_size)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| SkillError::ArchiveFormat {
            message: "central directory extends past the end of the archive".to_string(),
        })?;

    let truncated =
        || SkillError::ArchiveFormat { message: "truncated central directory record".to_string() };
    let mut names = Vec::with_capacity(total_entries.min(MAX_ENTRIES + 1));
    let mut cursor = directory_offset;
    for _ in 0..total_entries {
        if cursor + CENTRAL_HEADER_LEN > directory_end {
            return Err(truncated());
        }
        if bytes[cursor..cursor + 4] != CENTRAL_HEADER_SIGNATURE {
            return Err(SkillError::ArchiveFormat {
                message: "malformed central directory header".to_string(),
            });
        }
        let name_len = read_u16(cursor + 28);
        let extra_len = read_u16(cursor + 30);
        let comment_len = read_u16(cursor + 32);
        let name_start = cursor + CENTRAL_HEADER_LEN;
        let name_end = name_start + name_len;
        if name_end > directory_end {
            return Err(truncated());
        }
        names.push(String::from_utf8_lossy(&bytes[name_start..name_end]).into_owned());
        cursor = name_end + extra_len + comment_len;
    }
    Ok(names)
}

fn validate_entry_name(name: &str) -> SkillResult<()> {
    if name.is_empty() {
        return Err(SkillError::ArchiveFormat { message: "entry with an empty name".to_string() });
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err(SkillError::ArchiveAbsolutePath { name: name.to_string() });
    }
    let components: Vec<&str> =
        name.split(['/', '\\']).filter(|component| !component.is_empty()).collect();
    if components.contains(&"..") {
        return Err(SkillError::ArchivePathTraversal { name: name.to_string() });
    }
    // Directory entries end in a separator; their last component is itself a
    // directory level. For files, the last component is the file name.
    let is_dir_entry = name.ends_with('/') || name.ends_with('\\');
    let depth = if is_dir_entry { components.len() } else { components.len().saturating_sub(1) };
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(SkillError::ArchiveDepthExceeded {
            name: name.to_string(),
            depth,
            limit: MAX_DIRECTORY_DEPTH,
        });
    }
    Ok(())
}
