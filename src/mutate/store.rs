//  src/mutate/store.rs

//! Before a run mutates a file it saves the original, and this module keeps
//! those saved copies.
//!
//! The original bytes are saved under `<log-dir>/.originals/`, and a line
//! naming the copy and its destination is appended to
//! `<log-dir>/.originals/MANIFEST`. A run that ends normally, or on Ctrl-C,
//! restores the file itself and removes the copy. A run that is killed outright
//! cannot, and it leaves the copy behind. `--restore` then puts every saved
//! copy back from the manifest, and the next run refuses to start while any
//! copy remains.
//!
//! Each root's copies go under a tag of the form `@<root name>-<8 hex digits>`,
//! so two roots with the same name, or the same relative path under two roots,
//! never collide. The tag only has to be unique within one run, since
//! `--restore` reads destinations from the manifest rather than from the tag.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

pub(crate) const MANIFEST: &str = "MANIFEST";

// ===========================================================================
// Writing files
// ===========================================================================

/// Writes `bytes` to `path` and sets its modification time to now.
///
/// Cargo decides what to rebuild by comparing modification times, so a file
/// restored with its old time could leave a build of the mutated text in place.
/// Setting the time to now makes Cargo rebuild the file after both the mutation
/// and the restore.
pub(crate) fn write_now(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::write(path, bytes)?;
    touch(path)
}

/// Sets a file's modification time to now without changing its contents.
pub(crate) fn touch(path: &Path) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::now())
}

// ===========================================================================
// The store
// ===========================================================================

fn tag(root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    root.hash(&mut hasher);
    let name = root
        .file_name()
        .map_or_else(|| "root".to_owned(), |n| n.to_string_lossy().into_owned());
    format!("@{name}-{:08x}", hasher.finish() & 0xFFFF_FFFF)
}

/// A path's components joined with `/`, as the manifest records them.
fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Saves `original`, the bytes of `root/file`, records it in the manifest, and
/// returns the copy's path.
pub(crate) fn save(keep: &Path, root: &Path, file: &Path, original: &[u8]) -> io::Result<PathBuf> {
    let rel = Path::new(&tag(root)).join(file);
    let saved = keep.join(&rel);
    if let Some(parent) = saved.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&saved, original)?;
    let mut manifest = OpenOptions::new()
        .create(true)
        .append(true)
        .open(keep.join(MANIFEST))?;
    writeln!(
        manifest,
        "{}\t{}",
        slash_path(&rel),
        root.join(file).display()
    )?;
    manifest.sync_all()?;
    Ok(saved)
}

/// Removes a restored file's copy and its manifest line.
pub(crate) fn forget(keep: &Path, saved: &Path) -> io::Result<()> {
    fs::remove_file(saved)?;
    let rel = slash_path(saved.strip_prefix(keep).unwrap_or(saved));
    let manifest = keep.join(MANIFEST);
    let lines: Vec<String> = read_manifest(&manifest)?
        .into_iter()
        .filter(|(r, _)| *r != rel)
        .map(|(r, d)| format!("{r}\t{}", d.display()))
        .collect();
    if lines.is_empty() {
        fs::remove_file(&manifest)?;
    } else {
        fs::write(&manifest, lines.join("\n") + "\n")?;
    }
    Ok(())
}

fn read_manifest(manifest: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let text = match fs::read_to_string(manifest) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(rel, dst)| (rel.to_owned(), PathBuf::from(dst)))
        .collect())
}

fn copies(dir: &Path, keep: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            copies(&path, keep, out)?;
        } else if path != keep.join(MANIFEST) {
            out.push(path);
        }
    }
    Ok(())
}

/// Every saved copy still in the store, with its destination when the manifest
/// names one.
pub(crate) fn leftovers(keep: &Path) -> io::Result<Vec<(PathBuf, Option<PathBuf>)>> {
    let manifest = read_manifest(&keep.join(MANIFEST))?;
    let mut found = Vec::new();
    copies(keep, keep, &mut found)?;
    found.sort();
    Ok(found
        .into_iter()
        .map(|copy| {
            let rel = slash_path(copy.strip_prefix(keep).unwrap_or(&copy));
            let dst = manifest
                .iter()
                .find(|(r, _)| *r == rel)
                .map(|(_, d)| d.clone());
            (copy, dst)
        })
        .collect())
}

/// Removes the store when it holds no saved copies.
pub(crate) fn remove_if_empty(keep: &Path) -> io::Result<()> {
    if keep.is_dir() && leftovers(keep)?.is_empty() {
        fs::remove_dir_all(keep)?;
    }
    Ok(())
}

/// Puts every saved copy back byte for byte and empties the store, which is
/// what `--restore` does. A copy whose destination the manifest does not name
/// is kept and reported. Returns the restored paths.
pub(crate) fn restore_all(keep: &Path) -> io::Result<Vec<PathBuf>> {
    let mut restored = Vec::new();
    let mut unplaced = Vec::new();
    for (copy, dst) in leftovers(keep)? {
        let Some(dst) = dst else {
            unplaced.push(copy);
            continue;
        };
        let bytes = fs::read(&copy)?;
        write_now(&dst, &bytes)?;
        if fs::read(&dst)? != bytes {
            return Err(io::Error::other(format!(
                "{} did not restore byte for byte; its copy is kept at {}",
                dst.display(),
                copy.display()
            )));
        }
        forget(keep, &copy)?;
        restored.push(dst);
    }
    if !unplaced.is_empty() {
        let names: Vec<String> = unplaced.iter().map(|p| p.display().to_string()).collect();
        return Err(io::Error::other(format!(
            "the manifest names no destination for {}; restore these by hand",
            names.join(", ")
        )));
    }
    remove_if_empty(keep)?;
    Ok(restored)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::{forget, leftovers, restore_all, save, tag};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ostiarius-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("root/src")).expect("temp dir");
        dir
    }

    #[test]
    fn save_and_forget_round_trip() {
        let dir = temp("round-trip");
        let keep = dir.join("logs/.originals");
        let saved = save(
            &keep,
            &dir.join("root"),
            Path::new("src/lib.rs"),
            b"original",
        )
        .expect("saved");
        assert_eq!(fs::read(&saved).expect("copy"), b"original");
        assert_eq!(leftovers(&keep).expect("listed").len(), 1);
        forget(&keep, &saved).expect("forgotten");
        assert!(leftovers(&keep).expect("listed").is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_lines_removed() {
        let dir = temp("manifest");
        let keep = dir.join(".originals");
        let root = dir.join("root");
        let a = save(&keep, &root, Path::new("src/a.rs"), b"a").expect("saved");
        let _b = save(&keep, &root, Path::new("src/b.rs"), b"b").expect("saved");
        forget(&keep, &a).expect("forgotten");
        let manifest = fs::read_to_string(keep.join("MANIFEST")).expect("manifest");
        assert_eq!(manifest.lines().count(), 1);
        assert!(manifest.contains("src/b.rs\t"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_all_puts_bytes_back() {
        let dir = temp("restore");
        let keep = dir.join(".originals");
        let root = dir.join("root");
        fs::write(root.join("src/lib.rs"), b"mutated").expect("write");
        save(&keep, &root, Path::new("src/lib.rs"), b"original\r\n").expect("saved");
        let restored = restore_all(&keep).expect("restored");
        assert_eq!(restored, [root.join("src/lib.rs")]);
        assert_eq!(
            fs::read(root.join("src/lib.rs")).expect("read"),
            b"original\r\n"
        );
        assert!(!keep.exists(), "the empty store is removed");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_all_sets_mtime() {
        let dir = temp("mtime");
        let keep = dir.join(".originals");
        let root = dir.join("root");
        let target = root.join("src/lib.rs");
        fs::write(&target, b"mutated").expect("write");
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(&target)
            .expect("open")
            .set_modified(old)
            .expect("set");
        save(&keep, &root, Path::new("src/lib.rs"), b"original").expect("saved");
        restore_all(&keep).expect("restored");
        let modified = fs::metadata(&target)
            .expect("meta")
            .modified()
            .expect("mtime");
        assert!(modified > old + std::time::Duration::from_secs(1_000_000));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn leftovers_listed() {
        let dir = temp("leftovers");
        let keep = dir.join(".originals");
        save(&keep, &dir.join("root"), Path::new("src/lib.rs"), b"x").expect("saved");
        let found = leftovers(&keep).expect("listed");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].1.as_deref(),
            Some(dir.join("root/src/lib.rs").as_path())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tags_differ_for_same_basename_roots() {
        let a = tag(Path::new("/work/one/demo"));
        let b = tag(Path::new("/work/two/demo"));
        assert!(a.starts_with("@demo-") && b.starts_with("@demo-"));
        assert_ne!(a, b);
    }
}
