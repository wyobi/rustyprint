//! Writes PDF objects to a writer as they are finished, and the cross-reference table last.

use std::io::{self, Write};

use pdf_writer::{Chunk, Ref};

/// The file header: the version, and a comment of bytes that mark the file as binary.
const HEADER: &[u8] = b"%PDF-1.7\n%\x80\x80\x80\x80\n\n";

/// The largest byte offset an entry of a cross-reference table can hold: ten digits.
const MAX_OFFSET: u64 = 9_999_999_999;

/// A PDF file that is written one object at a time. Only where each object starts is kept.
pub(crate) struct Sink<W> {
    writer: W,
    /// The bytes written so far, which is where the next object starts.
    written: u64,
    /// Where each object starts, by its number; the first place is for object zero, which
    /// is never written.
    offsets: Vec<Option<u64>>,
}

impl<W: Write> Sink<W> {
    /// Starts a file in `writer`.
    pub fn new(mut writer: W) -> io::Result<Self> {
        writer.write_all(HEADER)?;
        Ok(Self {
            writer,
            written: HEADER.len() as u64,
            offsets: vec![None],
        })
    }

    /// Writes the one object that `build` puts into a chunk.
    pub fn object(&mut self, build: impl FnOnce(&mut Chunk)) -> io::Result<()> {
        let mut chunk = Chunk::new();
        build(&mut chunk);
        let mut ids = chunk.refs();
        let (Some(id), None) = (ids.next(), ids.next()) else {
            unreachable!("objects are written one at a time");
        };
        let number = id.get() as usize;
        if self.offsets.len() <= number {
            self.offsets.resize(number + 1, None);
        }
        assert!(
            self.offsets[number].is_none(),
            "object {number} is written once"
        );
        self.offsets[number] = Some(self.written);
        self.writer.write_all(chunk.as_bytes())?;
        self.written += chunk.len() as u64;
        Ok(())
    }

    /// The writer, which has every object written so far.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Writes the cross-reference table and the trailer, and returns the writer.
    pub fn finish(mut self, catalog: Ref, info: Ref) -> io::Result<W> {
        if self.written > MAX_OFFSET {
            return Err(io::Error::other(
                "the PDF is larger than a cross-reference table can address",
            ));
        }
        let count = self.offsets.len();
        // Numbers that were taken and not written form the list of free objects, which
        // starts and ends at object zero.
        let mut next_free = vec![0; count];
        let mut free = 0;
        for number in (0..count).rev() {
            if self.offsets[number].is_none() {
                next_free[number] = free;
                free = number;
            }
        }
        let mut table = Vec::with_capacity(20 * count + 128);
        write!(table, "xref\n0 {count}\n")?;
        for (number, offset) in self.offsets.iter().enumerate() {
            match offset {
                Some(offset) => write!(table, "{offset:010} 00000 n\r\n")?,
                None => {
                    let generation = if number == 0 { 65535 } else { 0 };
                    write!(table, "{:010} {generation:05} f\r\n", next_free[number])?;
                }
            }
        }
        write!(
            table,
            "trailer\n<<\n  /Size {count}\n  /Root {} 0 R\n  /Info {} 0 R\n>>\nstartxref\n{}\n%%EOF",
            catalog.get(),
            info.get(),
            self.written
        )?;
        self.writer.write_all(&table)?;
        self.writer.flush()?;
        Ok(self.writer)
    }
}
