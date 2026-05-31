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
    /// Existing callers stay on the unbounded version.
    pub fn drain(&mut self, budget: Duration) -> Result<Vec<Event>> {
        self.drain_up_to(budget, usize::MAX)
    }

    /// Same as `drain` but stops after `max_lines` parsed events.
    /// Lets the UI redraw between chunks instead of blocking on huge files.
    pub fn drain_up_to(&mut self, budget: Duration, max_lines: usize) -> Result<Vec<Event>> {
        let mut out = Vec::new();
        self.read_available(&mut out, max_lines)?;
        if !self.follow || out.len() >= max_lines {
            return Ok(out);
        }
        // Poll for new bytes until budget exhausted or something shows up.
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
            let before = out.len();
            self.reader = new_reader;
            self.read_available(&mut out, max_lines)?;
            if out.len() > before {
                break;
            }
        }
        Ok(out)
    }

    fn read_available(&mut self, out: &mut Vec<Event>, max_lines: usize) -> Result<()> {
        while out.len() < max_lines {
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
        Ok(())
    }
}
