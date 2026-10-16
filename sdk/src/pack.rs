//! The RADP v1 plugin package: `RADP`, u32 BE version (1), u32 BE manifest length, u64 BE
//! executable length, the manifest JSON, the native executable. Exactly two blobs: no names,
//! paths, compression, optional entries or trailing bytes.
use std::fmt;

pub const MAGIC: &[u8; 4] = b"RADP";
pub const VERSION: u32 = 1;
/// Manifest sizes accepted: 1..=65536 bytes.
pub const MAX_MANIFEST: usize = 65_536;
/// Executable sizes accepted: 1..=64 MiB.
pub const MAX_EXECUTABLE: u64 = 64 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum PackError {
    /// The manifest is empty or larger than [`MAX_MANIFEST`]; carries its size.
    ManifestSize(u64),
    /// The executable is empty or larger than [`MAX_EXECUTABLE`]; carries its size.
    ExecutableSize(u64),
}

impl fmt::Display for PackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestSize(n) => {
                write!(f, "manifest is {n} bytes; it must be 1..={MAX_MANIFEST}")
            }
            Self::ExecutableSize(n) => {
                write!(
                    f,
                    "executable is {n} bytes; it must be 1..={MAX_EXECUTABLE}"
                )
            }
        }
    }
}
impl std::error::Error for PackError {}

/// Checks the two sizes without holding the data, so oversized inputs can be refused unread.
pub fn check_sizes(manifest: u64, executable: u64) -> Result<(), PackError> {
    if !(1..=MAX_MANIFEST as u64).contains(&manifest) {
        return Err(PackError::ManifestSize(manifest));
    }
    if !(1..=MAX_EXECUTABLE).contains(&executable) {
        return Err(PackError::ExecutableSize(executable));
    }
    Ok(())
}

/// The package bytes for `manifest` and `executable`, after checking both sizes.
pub fn encode(manifest: &[u8], executable: &[u8]) -> Result<Vec<u8>, PackError> {
    check_sizes(manifest.len() as u64, executable.len() as u64)?;
    let mut package = Vec::with_capacity(20 + manifest.len() + executable.len());
    package.extend_from_slice(MAGIC);
    package.extend_from_slice(&VERSION.to_be_bytes());
    package.extend_from_slice(&(manifest.len() as u32).to_be_bytes());
    package.extend_from_slice(&(executable.len() as u64).to_be_bytes());
    package.extend_from_slice(manifest);
    package.extend_from_slice(executable);
    Ok(package)
}
