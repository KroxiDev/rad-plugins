//! The RADP v1 container: `RADP`, u32 BE version 1, u32 BE manifest length, u64 BE executable
//! length, the manifest, the executable. Nothing else.
use rad_plugin_sdk::pack::{self, PackError};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn expected(manifest: &[u8], executable: &[u8]) -> Vec<u8> {
    let mut bytes = b"RADP".to_vec();
    bytes.extend_from_slice(&[0, 0, 0, 1]);
    bytes.extend_from_slice(&(manifest.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&(executable.len() as u64).to_be_bytes());
    bytes.extend_from_slice(manifest);
    bytes.extend_from_slice(executable);
    bytes
}

#[test]
fn the_package_is_exactly_the_documented_header_followed_by_the_two_blobs() {
    let manifest = br#"{"contract":1}"#;
    let executable = b"\x7fELF-not-really";
    let package = pack::encode(manifest, executable).unwrap();
    // Header laid out by hand: 4 + 4 + 4 + 8 bytes.
    assert_eq!(&package[..4], b"RADP");
    assert_eq!(package[4..8], [0, 0, 0, 1]);
    assert_eq!(package[8..12], [0, 0, 0, 14]);
    assert_eq!(
        package[12..20],
        [0, 0, 0, 0, 0, 0, 0, 15],
        "executable is 15 bytes"
    );
    assert_eq!(
        package.len(),
        20 + manifest.len() + executable.len(),
        "no trailing bytes"
    );
    assert_eq!(package, expected(manifest, executable));
}

#[test]
fn sizes_outside_the_bounds_are_refused_and_the_bounds_themselves_accepted() {
    assert_eq!(pack::encode(b"", b"x"), Err(PackError::ManifestSize(0)));
    assert_eq!(
        pack::encode(&vec![b'm'; 65_537], b"x"),
        Err(PackError::ManifestSize(65_537))
    );
    assert_eq!(pack::encode(b"{}", b""), Err(PackError::ExecutableSize(0)));
    assert!(pack::encode(&vec![b'm'; 65_536], b"x").is_ok());
    assert!(pack::encode(b"{}", &vec![0u8; 64 * 1024 * 1024]).is_ok());
    assert_eq!(
        pack::encode(b"{}", &vec![0u8; 64 * 1024 * 1024 + 1]),
        Err(PackError::ExecutableSize(64 * 1024 * 1024 + 1))
    );
    // Sizes alone can be checked without holding the data.
    assert_eq!(
        pack::check_sizes(2, 64 * 1024 * 1024 + 1),
        Err(PackError::ExecutableSize(64 * 1024 * 1024 + 1))
    );
    assert_eq!(pack::check_sizes(0, 1), Err(PackError::ManifestSize(0)));
    assert_eq!(pack::check_sizes(1, 1), Ok(()));
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pack-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pack_cli(args: &[&Path]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_radp-pack"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn the_cli_writes_the_package_for_a_manifest_and_an_executable() {
    let dir = Scratch::new("ok");
    fs::write(dir.path("manifest.json"), br#"{"id":"x"}"#).unwrap();
    fs::write(dir.path("plugin"), b"binary").unwrap();
    let out = pack_cli(&[
        &dir.path("manifest.json"),
        &dir.path("plugin"),
        &dir.path("x.radp"),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read(dir.path("x.radp")).unwrap(),
        expected(br#"{"id":"x"}"#, b"binary")
    );
}

#[test]
fn the_cli_refuses_bad_input_without_leaving_a_package_behind() {
    let dir = Scratch::new("bad");
    fs::write(dir.path("manifest.json"), b"{}").unwrap();
    fs::write(dir.path("empty"), b"").unwrap();
    fs::write(dir.path("plugin"), b"binary").unwrap();
    // Sparse file one byte over the limit: refused from its length, never read.
    let huge = fs::File::create(dir.path("huge")).unwrap();
    huge.set_len(64 * 1024 * 1024 + 1).unwrap();
    let out = dir.path("out.radp");
    for (manifest, executable) in [
        ("manifest.json", "empty"),
        ("manifest.json", "huge"),
        ("manifest.json", "missing"),
        ("missing", "plugin"),
        ("empty", "plugin"),
    ] {
        let result = pack_cli(&[&dir.path(manifest), &dir.path(executable), &out]);
        assert_eq!(result.status.code(), Some(1), "{manifest} + {executable}");
        assert!(
            !result.stderr.is_empty(),
            "the failure is explained on stderr"
        );
        assert!(!out.exists(), "{manifest} + {executable} left a package");
    }
    // Wrong argument counts are a usage error.
    assert_eq!(pack_cli(&[]).status.code(), Some(2));
    assert_eq!(
        pack_cli(&[&dir.path("manifest.json"), &dir.path("plugin")])
            .status
            .code(),
        Some(2)
    );
}

fn names_in(dir: &Scratch) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_write_that_fails_halfway_leaves_the_previous_package_and_no_scratch_file() {
    let dir = Scratch::new("halfway");
    fs::write(dir.path("manifest.json"), b"{}").unwrap();
    fs::write(dir.path("plugin"), vec![7u8; 2 * 1024 * 1024]).unwrap();
    fs::write(dir.path("out.radp"), b"PREVIOUS").unwrap();
    // The shell caps file sizes at 256 KiB for the packer and ignores the signal that would kill
    // it, so the write fails with "file too large" part of the way through the 2 MiB package.
    let result = Command::new("sh")
        .arg("-c")
        .arg("ulimit -f 512; trap '' XFSZ; exec \"$0\" \"$@\"")
        .arg(env!("CARGO_BIN_EXE_radp-pack"))
        .args([
            dir.path("manifest.json"),
            dir.path("plugin"),
            dir.path("out.radp"),
        ])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!result.stderr.is_empty());
    assert_eq!(
        fs::read(dir.path("out.radp")).expect("the previous package is still there"),
        b"PREVIOUS"
    );
    assert_eq!(
        names_in(&dir),
        ["manifest.json", "out.radp", "plugin"],
        "no scratch file left"
    );
}

#[test]
fn a_package_replacing_another_appears_whole_or_not_at_all() {
    let dir = Scratch::new("whole");
    fs::write(dir.path("manifest.json"), b"{}").unwrap();
    fs::write(dir.path("plugin"), vec![9u8; 24 * 1024 * 1024]).unwrap();
    fs::write(dir.path("out.radp"), b"PREVIOUS").unwrap();
    let new_len = (20 + 2 + 24 * 1024 * 1024) as u64;
    let mut child = Command::new(env!("CARGO_BIN_EXE_radp-pack"))
        .args([
            dir.path("manifest.json"),
            dir.path("plugin"),
            dir.path("out.radp"),
        ])
        .spawn()
        .unwrap();
    // Watch the destination while the packer works: it is the old file or the finished one.
    let mut seen = std::collections::BTreeSet::new();
    while child.try_wait().unwrap().is_none() {
        if let Ok(meta) = fs::metadata(dir.path("out.radp")) {
            seen.insert(meta.len());
        }
    }
    assert!(child.wait().unwrap().success());
    seen.insert(fs::metadata(dir.path("out.radp")).unwrap().len());
    assert!(
        seen.iter().all(|&len| len == 8 || len == new_len),
        "partial package observed, sizes seen: {seen:?}"
    );
    assert_eq!(
        fs::read(dir.path("out.radp")).unwrap().len() as u64,
        new_len
    );
    assert_eq!(
        names_in(&dir),
        ["manifest.json", "out.radp", "plugin"],
        "no scratch file left"
    );
}
