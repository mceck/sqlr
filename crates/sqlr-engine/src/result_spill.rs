//! Blocking file work, separate from the result and engine locks.

use std::sync::Arc;

use parking_lot::Mutex;
use sqlr_core::{DriverError, ErrorKind, ResultChunk};

use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};

#[derive(Debug)]
struct SpillFile(File);

impl SpillFile {
    fn new() -> io::Result<Self> {
        let file = tempfile::tempfile()?;
        // On Linux `O_TMPFILE` creates the file with the umask default (0644), not 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self(file))
    }

    fn store(self: &Arc<Self>, chunk: &ResultChunk) -> io::Result<ResultChunk> {
        let Some(encoded) = chunk.encode_spill() else {
            return Ok(chunk.clone());
        };
        let mut file = &self.0;
        let start = file.seek(SeekFrom::End(0))?;
        file.write_all(encoded.bytes())?;
        // SAFETY: this anonymous file has no public handle or path. SpillStore
        // serializes appends and maps only completed ranges; later writes append
        // beyond them. The file is never truncated or overwritten, and mappings
        // retain its handle until their last reader drops them.
        let map = unsafe {
            MmapOptions::new()
                .offset(start)
                .len(encoded.bytes().len())
                .map(&self.0)?
        };
        encoded
            .into_chunk(Arc::new(Backing {
                map,
                _file: self.clone(),
            }))
            .map_err(io::Error::other)
    }
}

struct Backing {
    // Drop the mapping before the last file handle, including on Windows.
    map: Mmap,
    _file: Arc<SpillFile>,
}

impl AsRef<[u8]> for Backing {
    fn as_ref(&self) -> &[u8] {
        &self.map
    }
}

impl std::fmt::Debug for Backing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpillBacking")
            .field("bytes", &self.map.len())
            .finish()
    }
}

#[derive(Default)]
pub(crate) struct SpillStore(Mutex<Option<Arc<SpillFile>>>);

impl SpillStore {
    pub async fn store(
        self: &Arc<Self>,
        chunk: Arc<ResultChunk>,
    ) -> Result<ResultChunk, DriverError> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut file = store.0.lock();
            if file.is_none() {
                *file = Some(Arc::new(SpillFile::new()?));
            }
            file.as_ref()
                .ok_or_else(|| std::io::Error::other("missing spill storage"))?
                .store(&chunk)
        })
        .await
        .map_err(|_| DriverError::new(ErrorKind::Other, "Result spill worker failed"))?
        .map_err(|error| {
            DriverError::new(
                ErrorKind::Other,
                format!("Could not spill query results to temporary storage: {error}"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlr_core::{Cell, ChunkBuilder, ColumnKind, ColumnMeta, DisplayHint};

    fn chunk(value: &str) -> ResultChunk {
        let columns = vec![ColumnMeta::new(
            "value",
            "text",
            ColumnKind::Text,
            DisplayHint::Plain,
        )]
        .into();
        let mut builder = ChunkBuilder::new(columns);
        builder.push_row(&[Cell::Text(value)]).unwrap();
        builder.finish()
    }

    #[test]
    fn appends_keep_earlier_mappings_valid_and_last_reader_releases_file() {
        let file = Arc::new(SpillFile::new().unwrap());
        let weak = Arc::downgrade(&file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(file.0.metadata().unwrap().permissions().mode() & 0o077, 0);
        }
        let first = file.store(&chunk("first 東京😀")).unwrap();
        let second = file.store(&chunk("second")).unwrap();
        let snapshot = first.clone();
        drop(file);
        assert_eq!(first.cell(0, 0), Cell::Text("first 東京😀"));
        assert_eq!(second.cell(0, 0), Cell::Text("second"));
        drop(first);
        drop(second);
        assert!(weak.upgrade().is_some());
        assert_eq!(snapshot.cell(0, 0), Cell::Text("first 東京😀"));
        drop(snapshot);
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn file_write_failure_is_reported_and_original_rows_remain_readable() {
        let temporary = tempfile::NamedTempFile::new().unwrap();
        let read_only = Arc::new(SpillFile(File::open(temporary.path()).unwrap()));
        let store = Arc::new(SpillStore(Mutex::new(Some(read_only))));
        let original = Arc::new(chunk("keep these rows"));
        let error = store.store(original.clone()).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Other);
        assert!(error.message.contains("Could not spill query results"));
        assert_eq!(original.cell(0, 0), Cell::Text("keep these rows"));
    }
}
