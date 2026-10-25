use crate::binary::Input;
use crate::{Error, Model, Store};
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom};
use std::path::Path;

/// A binary log in a file, that a [`Store`] appends to. Only the state is
/// kept in memory, not the log, so memory use depends on the size of the
/// state, not on how long the log has grown.
///
/// [`open`](LogFile::open) replays the file as it's read, and
/// [`append`](LogFile::append) writes the events logged since then and
/// forgets them. If writing fails, the file is cut back to where it was, so
/// it always holds whole events.
///
/// ```no_run
/// use replayable::{model, LogFile};
///
/// model! { pub World { User { name: String } } }
///
/// let (mut file, mut store) = LogFile::open::<World>("world.rplb")?;
/// let u = store.create(User { name: "a".into() });
/// store.pin(u);
/// file.append(&mut store)?;
/// file.sync()?; // to be sure it's on disk, for example before reporting success
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct LogFile {
    file: File,
    /// The length of the file: the end of the last whole event.
    len: u64,
    truncated: u64,
    /// The store it was opened with, and how many of its events are in the
    /// file.
    mark: (u64, usize),
}

impl LogFile {
    /// Opens the binary log at `path`, or creates an empty one, and replays
    /// it into a new store. The events aren't kept in memory, as with
    /// [`Store::resume_binary`].
    ///
    /// If the file ends in the middle of an event, as it can after a power
    /// loss while it was being written, that event is cut off the file, and
    /// [`truncated`](LogFile::truncated) says how many bytes were removed.
    /// Anything else wrong with the file is an [`Error`].
    pub fn open<M: Model>(path: impl AsRef<Path>) -> Result<(LogFile, Store<M>), Error> {
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
        let size = file.metadata()?.len();
        let (store, end) = Store::load_binary(Input::Stream(&mut file), false, true)?;
        let mut log = LogFile { file, len: end.complete, truncated: 0, mark: store.storage_mark() };
        if end.cut_off {
            log.truncated = size - end.complete;
            log.file.set_len(end.complete)?;
        }
        log.file.seek(SeekFrom::Start(log.len))?;
        Ok((log, store))
    }

    /// Writes the events that aren't in the file yet (see
    /// [`Store::flush_binary`]), and forgets them. They are written in one
    /// go when there aren't many, but not synced; see [`sync`](LogFile::sync).
    ///
    /// If writing fails, the file is cut back to its length before, and the
    /// store keeps the events, so `append` can be called again.
    ///
    /// Fails without writing anything if `store` isn't the one returned by
    /// [`open`](LogFile::open), or if some of its events were saved,
    /// flushed or forgotten without this file since the last append, since
    /// the file would then miss them.
    pub fn append<M: Model>(&mut self, store: &mut Store<M>) -> io::Result<()> {
        if store.storage_mark() != self.mark {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "this isn't the store the log file was opened with, or some of its events were saved, \
                 flushed or forgotten elsewhere since the last append, so the file would miss them",
            ));
        }
        match store.flush_binary(&mut self.file) {
            Ok(()) => {
                self.mark = store.storage_mark();
                self.len = self.file.stream_position()?;
                Ok(())
            }
            Err(e) => {
                // Best effort: if this fails too, the error from writing is
                // the one that matters, and `open` cuts off the partial event.
                if self.file.set_len(self.len).is_ok() {
                    let _ = self.file.seek(SeekFrom::Start(self.len));
                }
                Err(e)
            }
        }
    }

    /// Makes sure that what was appended is on the disk, not just handed to
    /// the operating system ([`File::sync_data`]). Separate from
    /// [`append`](LogFile::append), so that you choose how often: syncing
    /// is slow, and wears flash memory.
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// How many bytes of a cut-off event [`open`](LogFile::open) removed
    /// from the end of the file. Usually 0.
    pub fn truncated(&self) -> u64 {
        self.truncated
    }

    /// The length of the file, in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}
