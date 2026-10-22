//! A copy of the app's own log lines, kept in memory for the on-device log viewer.
//!
//! The logger writes through [`TeeWriter`]: every line still goes to stderr
//! (the journal, when run as a service) and is also pushed into a ring buffer
//! holding the most recent lines of this run. Nothing needs the journal, a
//! subprocess or extra privileges to show it.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::{Arc, Mutex, OnceLock};
use tracing_subscriber::fmt::MakeWriter;

/// How many lines the viewer can look back.
const CAPACITY: usize = 1000;
/// Longest line kept, so one huge message can't swamp the view.
const MAX_LINE_CHARS: usize = 400;

/// The most recent log lines, oldest first.
pub struct LogBuffer {
    lines: Mutex<VecDeque<String>>,
    capacity: usize,
}

impl LogBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            lines: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
        }
    }

    /// The buffer the logger writes to and the viewer reads from.
    pub fn global() -> &'static Arc<LogBuffer> {
        static GLOBAL: OnceLock<Arc<LogBuffer>> = OnceLock::new();
        GLOBAL.get_or_init(|| Arc::new(LogBuffer::new(CAPACITY)))
    }

    /// Add one line (without its newline). Colour codes are removed and very
    /// long lines clipped. The oldest line is dropped when the buffer is full.
    pub fn push(&self, line: &str) {
        let line = clip(&strip_ansi(line));
        // A poisoned lock only means another thread panicked while logging
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == self.capacity {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    /// All lines currently held, oldest first.
    pub fn snapshot(&self) -> Vec<String> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        lines.iter().cloned().collect()
    }
}

/// `MakeWriter` for the tracing subscriber: stderr plus the ring buffer.
#[derive(Clone)]
pub struct TeeMakeWriter {
    buffer: Arc<LogBuffer>,
}

impl TeeMakeWriter {
    pub fn new(buffer: Arc<LogBuffer>) -> Self {
        Self { buffer }
    }
}

impl<'a> MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> TeeWriter {
        TeeWriter {
            buffer: self.buffer.clone(),
            pending: Vec::new(),
        }
    }
}

/// Writes to stderr and collects the bytes into lines for the buffer.
pub struct TeeWriter {
    buffer: Arc<LogBuffer>,
    pending: Vec<u8>,
}

impl TeeWriter {
    /// Move every complete line out of `pending` into the buffer.
    fn push_complete_lines(&mut self) {
        while let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            self.buffer
                .push(String::from_utf8_lossy(&line[..line.len() - 1]).as_ref());
        }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // The copy for the viewer must not depend on stderr being writable
        let _ = std::io::stderr().write_all(data);
        self.pending.extend_from_slice(data);
        self.push_complete_lines();
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        Ok(())
    }
}

impl Drop for TeeWriter {
    fn drop(&mut self) {
        // A last line that never got its newline is still a line
        if !self.pending.is_empty() {
            let rest = std::mem::take(&mut self.pending);
            self.buffer.push(String::from_utf8_lossy(&rest).as_ref());
        }
    }
}

/// Remove ANSI escape sequences (colours) from a log line.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // A control sequence ends with a byte in @..~
            for end in chars.by_ref() {
                if ('@'..='~').contains(&end) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}...", &line[..end]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn writer(capacity: usize) -> (Arc<LogBuffer>, TeeMakeWriter) {
        let buffer = Arc::new(LogBuffer::new(capacity));
        (buffer.clone(), TeeMakeWriter::new(buffer))
    }

    #[test]
    fn keeps_only_the_most_recent_lines() {
        let buffer = LogBuffer::new(3);
        for i in 1..=5 {
            buffer.push(&format!("line {i}"));
        }
        assert_eq!(buffer.snapshot(), ["line 3", "line 4", "line 5"]);
    }

    #[test]
    fn splits_writes_into_lines() {
        let (buffer, make) = writer(10);
        {
            let mut w = make.make_writer();
            w.write_all(b"first\nsecond\nthi").unwrap();
            w.write_all(b"rd\n").unwrap();
        }
        assert_eq!(buffer.snapshot(), ["first", "second", "third"]);
    }

    #[test]
    fn a_last_line_without_newline_is_kept() {
        let (buffer, make) = writer(10);
        make.make_writer().write_all(b"no newline").unwrap();
        assert_eq!(buffer.snapshot(), ["no newline"]);
    }

    #[test]
    fn ansi_colour_codes_are_removed() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026-10-19\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m started"),
            "2026-10-19  INFO started"
        );
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("broken \u{1b}[3"), "broken ");
    }

    #[test]
    fn long_lines_are_clipped_on_a_character_boundary() {
        let buffer = LogBuffer::new(2);
        buffer.push(&"ä".repeat(MAX_LINE_CHARS + 50));
        let line = &buffer.snapshot()[0];
        assert_eq!(line.chars().count(), MAX_LINE_CHARS + 3);
        assert!(line.ends_with("..."));
    }

    #[test]
    fn works_as_a_tracing_writer() {
        let (buffer, make) = writer(10);
        let subscriber = tracing_subscriber::fmt()
            .with_writer(make)
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!("underrun on playback");
        });
        let lines = buffer.snapshot();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("WARN") && lines[0].contains("underrun on playback"));
    }
}
