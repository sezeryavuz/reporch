//! Atomic writes and advisory locking (CLAUDE.md, §7.2).
//!
//! > *"every file under `~/.reporch/` is written temp-then-rename. Nine sessions share that
//! > directory. `briefs/index.json` needs an advisory lock around allocate-and-write."*
//!
//! Both halves matter and they solve different problems. Temp-then-rename means a reader never
//! sees a half-written file. The lock means two sessions allocating a brief ID at the same moment
//! do not both get `0007`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fs4::{FileExt, TryLockError};

use crate::StoreError;

/// Write `bytes` to `path` atomically: a temp file in the same directory, flushed and synced,
/// then renamed over the target.
///
/// The temp file must share a directory with the target — `rename(2)` is only atomic within a
/// filesystem, and `/tmp` is routinely a different one.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().ok_or_else(|| StoreError::BadPath {
        path: path.to_path_buf(),
        detail: "has no parent directory".into(),
    })?;
    fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;

    let mut tmp = tempfile::Builder::new()
        .prefix(".reporch-tmp-")
        .tempfile_in(dir)
        .map_err(|e| StoreError::io(dir, e))?;
    tmp.write_all(bytes).map_err(|e| StoreError::io(path, e))?;
    tmp.as_file_mut()
        .sync_all()
        .map_err(|e| StoreError::io(path, e))?;
    tmp.persist(path)
        .map_err(|e| StoreError::io(path, e.error))?;
    Ok(())
}

/// Serialise `value` as pretty JSON and write it atomically.
///
/// Pretty, with a trailing newline, because §8's whole premise is that the user opens these files.
pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| StoreError::Serialise {
        path: path.to_path_buf(),
        source: e,
    })?;
    bytes.push(b'\n');
    write_atomic(path, &bytes)
}

/// Read and deserialise JSON.
pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, StoreError> {
    let bytes = fs::read(path).map_err(|e| StoreError::io(path, e))?;
    serde_json::from_slice(&bytes).map_err(|e| StoreError::Deserialise {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Read and deserialise JSON, or return `None` if the file does not exist.
///
/// A missing file is routinely *not* an error here: no cache yet, no project yet, no index yet.
pub fn read_json_opt<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| StoreError::Deserialise {
                    path: path.to_path_buf(),
                    source: e,
                })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::io(path, e)),
    }
}

pub fn write_string(path: &Path, s: &str) -> Result<(), StoreError> {
    write_atomic(path, s.as_bytes())
}

pub fn read_string(path: &Path) -> Result<String, StoreError> {
    fs::read_to_string(path).map_err(|e| StoreError::io(path, e))
}

/// An exclusive advisory lock held for the duration of a read-modify-write.
///
/// The lock lives in a sidecar `<name>.lock` rather than on the data file itself, so the data
/// file can still be replaced by rename while the lock is held — which is the entire point, since
/// renaming is how we write atomically.
#[derive(Debug)]
pub struct FileLock {
    file: File,
    path: PathBuf,
}

impl FileLock {
    /// Acquire an exclusive lock, blocking until it is available.
    pub fn acquire(target: &Path) -> Result<Self, StoreError> {
        let path = lock_path(target);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        }
        let file = File::options()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        FileExt::lock(&file).map_err(|e| StoreError::Lock {
            path: path.clone(),
            source: e,
        })?;
        Ok(Self { file, path })
    }

    /// Try to acquire without blocking. `None` means someone else holds it.
    pub fn try_acquire(target: &Path) -> Result<Option<Self>, StoreError> {
        let path = lock_path(target);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
        }
        let file = File::options()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| StoreError::io(&path, e))?;
        match FileExt::try_lock(&file) {
            Ok(()) => Ok(Some(Self { file, path })),
            // Someone else holds it. Not an error — the caller asked whether it was free.
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(StoreError::Lock { path, source: e }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Best-effort: the OS releases the lock when the fd closes anyway, so a failure here has
        // no consequence worth propagating out of a destructor.
        let _ = FileExt::unlock(&self.file);
    }
}

fn lock_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Read-modify-write a JSON file under an exclusive lock.
///
/// This is the shape `briefs/index.json` needs for allocate-and-write (§7.2): the read, the ID
/// allocation and the write all happen inside one lock, so two sessions filing a brief at the
/// same instant cannot both be handed the same ID.
pub fn update_json_locked<T, F, R>(
    path: &Path,
    default: impl FnOnce() -> T,
    f: F,
) -> Result<R, StoreError>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
    F: FnOnce(&mut T) -> Result<R, StoreError>,
{
    let _guard = FileLock::acquire(path)?;
    let mut value: T = read_json_opt(path)?.unwrap_or_else(default);
    let out = f(&mut value)?;
    write_json(path, &value)?;
    Ok(out)
}

/// Whether a path exists and is a regular file.
pub fn is_file(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Append a line to a file, creating it if needed. Used for the raw session streams (§10), which
/// are append-only and never read back mid-run, so they do not need the atomic dance.
pub fn append_line(path: &Path, line: &str) -> Result<(), StoreError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| StoreError::io(dir, e))?;
    }
    let mut f = File::options()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| StoreError::io(path, e))?;
    f.write_all(line.as_bytes())
        .and_then(|()| f.write_all(b"\n"))
        .map_err(|e| StoreError::io(path, e))
}

/// Read a file's first `n` bytes. Used for cheap sniffing without loading a large file.
pub fn read_prefix(path: &Path, n: usize) -> Result<Vec<u8>, StoreError> {
    let mut f = File::open(path).map_err(|e| StoreError::io(path, e))?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf).map_err(|e| StoreError::io(path, e))?;
    buf.truncate(read);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Thing {
        n: u32,
        s: String,
    }

    #[test]
    fn json_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a/b/thing.json");
        let t = Thing {
            n: 7,
            s: "hi".into(),
        };
        write_json(&p, &t).unwrap();
        assert_eq!(read_json::<Thing>(&p).unwrap(), t);
    }

    #[test]
    fn write_creates_missing_parent_directories() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("deeply/nested/path/x.json");
        write_json(
            &p,
            &Thing {
                n: 1,
                s: "x".into(),
            },
        )
        .unwrap();
        assert!(p.exists());
    }

    #[test]
    fn missing_file_reads_as_none_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("absent.json");
        assert_eq!(read_json_opt::<Thing>(&p).unwrap(), None);
    }

    #[test]
    fn malformed_json_is_an_error_naming_the_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.json");
        write_string(&p, "{not json").unwrap();
        let err = read_json_opt::<Thing>(&p).unwrap_err();
        assert!(err.to_string().contains("bad.json"), "got: {err}");
    }

    /// The point of temp-then-rename: a reader never observes a partial file. Overwriting an
    /// existing file must leave either the old content or the new, never a truncated mix.
    #[test]
    fn overwrite_is_atomic_from_a_readers_view() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        write_json(
            &p,
            &Thing {
                n: 1,
                s: "a".repeat(100_000),
            },
        )
        .unwrap();
        write_json(
            &p,
            &Thing {
                n: 2,
                s: "b".into(),
            },
        )
        .unwrap();
        let back: Thing = read_json(&p).unwrap();
        assert_eq!(back.n, 2);
        assert_eq!(back.s, "b");
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        for i in 0..5 {
            write_json(
                &p,
                &Thing {
                    n: i,
                    s: "x".into(),
                },
            )
            .unwrap();
        }
        let entries: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".reporch-tmp-"))
            .collect();
        assert!(entries.is_empty(), "leftover temp files: {entries:?}");
    }

    #[test]
    fn a_lock_is_exclusive_while_held() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.json");
        let held = FileLock::acquire(&p).unwrap();
        assert!(
            FileLock::try_acquire(&p).unwrap().is_none(),
            "a second acquire must fail while the first is held"
        );
        drop(held);
        assert!(
            FileLock::try_acquire(&p).unwrap().is_some(),
            "and succeed once released"
        );
    }

    #[test]
    fn the_lock_is_a_sidecar_not_the_data_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.json");
        let l = FileLock::acquire(&p).unwrap();
        assert_eq!(l.path(), d.path().join("index.json.lock"));
        // Writing the data file while the lock is held must still work — that is the whole
        // reason the lock is a sidecar.
        write_json(
            &p,
            &Thing {
                n: 1,
                s: "x".into(),
            },
        )
        .unwrap();
        assert_eq!(read_json::<Thing>(&p).unwrap().n, 1);
    }

    #[test]
    fn update_json_locked_reads_modifies_and_writes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("counter.json");
        for expected in 1..=3u32 {
            let got = update_json_locked(
                &p,
                || Thing {
                    n: 0,
                    s: String::new(),
                },
                |t| {
                    t.n += 1;
                    Ok(t.n)
                },
            )
            .unwrap();
            assert_eq!(got, expected);
        }
        assert_eq!(read_json::<Thing>(&p).unwrap().n, 3);
    }

    /// The real concern from §7.2: concurrent allocate-and-write must not hand out duplicate IDs.
    /// Threads share a process, so this exercises the read-modify-write window rather than the
    /// OS lock itself — which is exactly the window that loses IDs without a lock.
    #[test]
    fn concurrent_allocation_never_duplicates() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.json");
        write_json(
            &p,
            &Thing {
                n: 0,
                s: String::new(),
            },
        )
        .unwrap();

        let ids = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        std::thread::scope(|s| {
            for _ in 0..8 {
                let p = p.clone();
                let ids = ids.clone();
                s.spawn(move || {
                    for _ in 0..10 {
                        let id = update_json_locked(
                            &p,
                            || Thing {
                                n: 0,
                                s: String::new(),
                            },
                            |t| {
                                t.n += 1;
                                Ok(t.n)
                            },
                        )
                        .unwrap();
                        ids.lock().unwrap().push(id);
                    }
                });
            }
        });

        let mut ids = ids.lock().unwrap().clone();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "duplicate IDs were allocated");
        assert_eq!(ids.len(), 80);
        assert_eq!(read_json::<Thing>(&p).unwrap().n, 80);
    }

    #[test]
    fn append_line_accumulates() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("logs/x.jsonl");
        append_line(&p, "{\"a\":1}").unwrap();
        append_line(&p, "{\"a\":2}").unwrap();
        assert_eq!(read_string(&p).unwrap(), "{\"a\":1}\n{\"a\":2}\n");
    }
}
