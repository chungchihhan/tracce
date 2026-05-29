use crate::event::Event;
use anyhow::Result;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;

const FLUSH_EVERY: usize = 256;

pub struct Persist {
    inner: Mutex<Inner>,
}

struct Inner {
    file: BufWriter<File>,
    pending: usize,
}

impl Persist {
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { inner: Mutex::new(Inner { file: BufWriter::new(file), pending: 0 }) })
    }

    pub fn write(&self, ev: &Event) -> Result<()> {
        let line = serde_json::to_string(ev)?;
        let mut g = self.inner.lock().unwrap();
        writeln!(g.file, "{line}")?;
        g.pending += 1;
        if g.pending >= FLUSH_EVERY {
            g.file.flush()?;
            g.file.get_ref().sync_data()?;
            g.pending = 0;
        }
        Ok(())
    }

    pub fn flush(&self) -> Result<()> {
        let mut g = self.inner.lock().unwrap();
        g.file.flush()?;
        g.file.get_ref().sync_data()?;
        g.pending = 0;
        Ok(())
    }
}
