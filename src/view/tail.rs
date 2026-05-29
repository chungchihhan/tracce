use crate::event::Event;
use anyhow::Result;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Tail {
    path: PathBuf,
    reader: BufReader<std::fs::File>,
    follow: bool,
}

impl Tail {
    pub fn open(path: &Path, follow: bool) -> Result<Self> {
        let f = std::fs::File::open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            reader: BufReader::new(f),
            follow,
        })
    }

    /// Drain available lines, then if `follow`, wait up to `budget` for more.
    pub fn drain(&mut self, budget: Duration) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        // First: drain whatever is currently available.
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line)? {
                0 => break,
                _ => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
                            out.push(ev);
                        }
                    }
                }
            }
        }
        if !self.follow {
            return Ok(out);
        }
        // Poll until budget exhausted.
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(40));
            // Re-open the file to pick up writes flushed by another process/handle.
            // BufReader on the same file handle may not see appended data after EOF
            // without seeking back; re-opening is more reliable.
            let pos = self.reader.stream_position()?;
            let f = std::fs::File::open(&self.path)?;
            let mut new_reader = BufReader::new(f);
            new_reader.seek(SeekFrom::Start(pos))?;
            let mut line = String::new();
            if new_reader.read_line(&mut line)? > 0 {
                self.reader = new_reader;
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
                        out.push(ev);
                    }
                }
                // Drain any additional lines that may have appeared.
                loop {
                    let mut more = String::new();
                    match self.reader.read_line(&mut more)? {
                        0 => break,
                        _ => {
                            let trimmed = more.trim();
                            if !trimmed.is_empty() {
                                if let Ok(ev) = serde_json::from_str::<Event>(trimmed) {
                                    out.push(ev);
                                }
                            }
                        }
                    }
                }
                // If we found lines, we can stop polling early.
                break;
            }
            // Nothing new yet; continue polling with original reader.
        }
        Ok(out)
    }
}
