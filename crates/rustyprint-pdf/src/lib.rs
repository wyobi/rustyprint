//! Writes display lists to PDF: fonts, images, links and effects.
//!
//! Output is deterministic: the same document always produces the same bytes.

mod font;
mod sink;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::sync::Arc;

use miniz_oxide::deflate::compress_to_vec_zlib;
use pdf_writer::types::{
    ActionType, AnnotationType, BlendMode as PdfBlendMode, FunctionShadingType, LineCapStyle,
    LineJoinStyle, MaskType, TextRenderingMode,
};
use pdf_writer::writers::{Catalog, DocumentInfo};
use pdf_writer::{Content, Filter, Finish, Name, Rect, Ref, Str, TextStr};
use rustyprint_layout::display::Group;
use rustyprint_layout::display::LineStyle;
use rustyprint_layout::display::PrintMarks;
use rustyprint_layout::display::{
    ColorProfile, Destination, Document, Link, LinkTarget, Shading, ShadingKind,
};
use rustyprint_layout::{
    Color, DisplayItem, FillRule, Font, Image, ImageData, Path, PathSegment, Point, TextRun,
};

use crate::font::FontResource;
use crate::sink::Sink;

/// CSS pixels to PDF points.
const PX_TO_PT: f32 = 0.75;

/// Room for printer's marks outside the bleed, in points.
const MARKS_AREA: f32 = 24.0;

/// Zlib compression level for streams.
const COMPRESSION_LEVEL: u8 = 6;

/// The most threads that compress content streams.
const MAX_COMPRESSION_THREADS: usize = 8;

/// Pages whose content streams wait for compression, per compressing thread.
const PAGES_PER_THREAD: usize = 4;

/// Errors returned by [`write`] and [`Session`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to subset font {name}: {source}")]
    Subset {
        name: String,
        source: subsetter::Error,
    },
    #[error("failed to write the PDF: {0}")]
    Io(#[from] std::io::Error),
    #[error("a page that was kept open has no place for what is painted on it later")]
    NothingLater,
}

/// Writes `document` as a PDF file.
pub fn write(document: &Document) -> Result<Vec<u8>, Error> {
    let mut session = Session::new(Vec::new(), Setup::of(document))?;
    session.add_pages(document)?;
    session.finish()
}

/// What is the same for a whole PDF file, however many batches of pages it is written in.
#[derive(Debug, Clone, Copy, Default)]
pub struct Setup<'a> {
    /// Document title for the file's metadata.
    pub title: Option<&'a str>,
    /// The ICC profiles that native colors refer to by index.
    pub color_profiles: &'a [ColorProfile],
    /// The profile of `device-cmyk()` colors, an index into `color_profiles`; without one
    /// they are device CMYK.
    pub device_cmyk: Option<u32>,
}

impl<'a> Setup<'a> {
    /// The title and color profiles of `document`.
    pub fn of(document: &'a Document) -> Self {
        Self {
            title: document.title.as_deref(),
            color_profiles: &document.color_profiles,
            device_cmyk: document.device_cmyk,
        }
    }
}

/// One PDF file written a batch of pages at a time: each batch is written as it is added,
/// so its pages need not be kept.
///
/// Every object is one write to the writer, so a file is best wrapped in a
/// [`std::io::BufWriter`]. After an error the file is incomplete and the session cannot go
/// on.
pub struct Session<W: Write> {
    sink: Sink<W>,
    refs: Refs,
    catalog_id: Ref,
    page_tree_id: Ref,
    info_id: Ref,
    /// The pages written so far.
    page_ids: Vec<Ref>,
    /// The bookmarks of the pages written so far, for the outline.
    outline: Vec<OutlineEntry>,
    /// Fonts in order of first use, so object numbering is deterministic.
    fonts: Vec<FontResource>,
    /// Graphics states for each alpha value used, by alpha.
    alphas: Vec<(u8, Ref)>,
    /// Images in order of first use.
    images: Vec<ImageResource>,
    /// The images written so far by what they hold, so that an image that comes again in a
    /// later batch is written once without the session keeping its pixels.
    image_keys: HashMap<ImageKey, usize>,
    /// The images of the batch being written, by where they are in memory, so that an
    /// image a batch repeats is hashed once. Empty between batches.
    batch_images: Vec<(usize, bool, usize)>,
    /// How many of `images` are written: an image is written with the first page that
    /// uses it.
    images_written: usize,
    /// Shadings written so far, for unique resource names.
    shadings: usize,
    /// Groups written so far, for unique resource names.
    groups: usize,
    /// How native colors are written.
    palette: Palette,
    /// The pages that are painted on again before the file ends, by their place in the
    /// file.
    later: HashMap<usize, LaterPage>,
    /// The last page of the last batch, when it is kept open.
    open: Option<OpenPage>,
}

/// The objects of a page that is kept open, which are written when it is closed: its
/// content stream, the stream that paints on it later, its resources and its annotations.
#[derive(Clone, Copy)]
struct OpenIds {
    page: Ref,
    content: Ref,
    /// The stream that paints on the page later, if the page had something to paint
    /// later when its page object was written.
    later: Option<Ref>,
    resources: Ref,
    annotations: Ref,
}

/// A page that is kept open: its page object is written, and what it paints is not, since
/// the next batch may be that page with more on it.
struct OpenPage {
    ids: OpenIds,
    /// Its place in the file.
    index: usize,
    /// Its height in points.
    height: f32,
    /// Its content stream, not compressed.
    content: Vec<u8>,
    /// What the content stream uses.
    used: Resources,
    links: Vec<Link>,
    /// The pages of its batch, which its links point to.
    batch: Vec<(Ref, f32)>,
    /// Whether it is painted on again before the file ends.
    later: bool,
    /// The bookmarks to it.
    bookmarks: Vec<OutlineEntry>,
}

/// A page that is written and gets more painted on it before the file ends: the content
/// stream and the resource dictionary it has by number, which are still to be written.
struct LaterPage {
    content_id: Ref,
    resources_id: Ref,
    /// What the page's first content stream uses.
    used: Resources,
    /// The page's height in points.
    height: f32,
}

/// A bookmark whose target is resolved to a page object.
struct OutlineEntry {
    level: u8,
    title: String,
    page: Ref,
    /// The top edge in PDF coordinates.
    top: f32,
}

/// How native colors are written: the document's color profiles as ICC-based color spaces.
#[derive(Default)]
struct Palette {
    /// For each profile: its color space resource name, ICC stream and component count.
    profiles: Vec<(String, Ref, usize)>,
    /// The profile of `device-cmyk()` colors; without one they are device CMYK.
    device_cmyk: Option<u32>,
}

/// A native color as it is set in a content stream.
enum NativeFill<'a> {
    Cmyk([f32; 4]),
    /// A named ICC-based color space, and the color's components with their count.
    Icc(&'a str, [f32; 4], usize),
}

impl Palette {
    fn native(&self, native: rustyprint_layout::display::NativeColor) -> Option<NativeFill<'_>> {
        let components = native.components.map(|c| f32::from(c) / 65535.0);
        let space = if native.space == rustyprint_layout::display::DEVICE_CMYK {
            match self.device_cmyk {
                Some(index) => index,
                None => return Some(NativeFill::Cmyk(components)),
            }
        } else {
            native.space
        };
        let (name, _, count) = self.profiles.get(space as usize)?;
        Some(NativeFill::Icc(name, components, *count))
    }
}

/// Allocates PDF object IDs in sequence.
#[derive(Default)]
pub(crate) struct Refs(i32);

impl Refs {
    pub fn next(&mut self) -> Ref {
        self.0 += 1;
        Ref::new(self.0)
    }
}

/// How many threads compress content streams: one per core, up to
/// [`MAX_COMPRESSION_THREADS`].
fn compression_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_COMPRESSION_THREADS)
}

/// Compresses each stream, on up to `threads` threads; the results keep their order.
fn compress_all(streams: Vec<Vec<u8>>, threads: usize) -> Vec<Vec<u8>> {
    let compress = |data: &Vec<u8>| compress_to_vec_zlib(data, COMPRESSION_LEVEL);
    if threads < 2 || streams.len() < 2 {
        return streams.iter().map(compress).collect();
    }
    let per_thread = streams.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let workers: Vec<_> = streams
            .chunks(per_thread)
            .map(|chunk| scope.spawn(move || chunk.iter().map(compress).collect::<Vec<_>>()))
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| match worker.join() {
                Ok(compressed) => compressed,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    })
}

impl<W: Write> Session<W> {
    /// Starts a PDF file in `writer`.
    pub fn new(writer: W, setup: Setup<'_>) -> Result<Self, Error> {
        let mut sink = Sink::new(writer)?;
        let mut refs = Refs::default();
        // The catalog and the page tree are written last, when the pages are known.
        let catalog_id = refs.next();
        let page_tree_id = refs.next();
        // Color profiles are written once, and every content stream can use them.
        let mut palette = Palette {
            profiles: Vec::with_capacity(setup.color_profiles.len()),
            device_cmyk: setup.device_cmyk,
        };
        for (index, profile) in setup.color_profiles.iter().enumerate() {
            let id = refs.next();
            let data = compress_to_vec_zlib(&profile.data, COMPRESSION_LEVEL);
            sink.object(|chunk| {
                let mut stream = chunk.icc_profile(id, &data);
                stream.filter(Filter::FlateDecode);
                stream.n(profile.components as i32);
                let alternate = match profile.components {
                    1 => Name(b"DeviceGray"),
                    4 => Name(b"DeviceCMYK"),
                    _ => Name(b"DeviceRGB"),
                };
                stream.alternate_name(alternate);
            })?;
            palette
                .profiles
                .push((format!("Cs{index}"), id, profile.components));
        }
        let info_id = refs.next();
        sink.object(|chunk| {
            let mut info = chunk.indirect(info_id).start::<DocumentInfo>();
            info.producer(TextStr("rustyprint"));
            if let Some(title) = setup.title {
                info.title(TextStr(title));
            }
        })?;
        Ok(Self {
            sink,
            refs,
            catalog_id,
            page_tree_id,
            info_id,
            page_ids: Vec::new(),
            outline: Vec::new(),
            fonts: Vec::new(),
            alphas: Vec::new(),
            images: Vec::new(),
            image_keys: HashMap::new(),
            batch_images: Vec::new(),
            images_written: 0,
            shadings: 0,
            groups: 0,
            palette,
            later: HashMap::new(),
            open: None,
        })
    }

    /// How many pages are written so far.
    pub fn page_count(&self) -> usize {
        self.page_ids.len()
    }

    /// The writer. When [`Session::add_pages`] returns, the writer has every byte of the
    /// file so far, so what it holds can be taken: the session keeps count of the bytes
    /// itself.
    pub fn get_mut(&mut self) -> &mut W {
        self.sink.get_mut()
    }

    /// Writes the pages of `document` after the pages already written, with the images and
    /// links they use. The targets of its links and bookmarks are pages of `document`. Its
    /// title and color profiles are not used: those of the [`Setup`] hold for the file.
    pub fn add_pages(&mut self, document: &Document) -> Result<(), Error> {
        self.add_pages_painted_later(document, &[])
    }

    /// Writes the pages of `document` as [`Session::add_pages`] does. The pages at `later`,
    /// counted in `document` from zero, get a second content stream that is not written
    /// yet: [`Session::paint_later`] writes it, with what is only known at the end of
    /// the file. Of such a page the session keeps which resources it uses.
    pub fn add_pages_painted_later(
        &mut self,
        document: &Document,
        later: &[usize],
    ) -> Result<(), Error> {
        self.add(document, later, false)
    }

    /// Writes the pages of `document` as [`Session::add_pages_painted_later`] does, and
    /// keeps the last of them open: its page object is written, so it is a page of the file
    /// that links can point to, and what it paints is kept until the page is closed. The
    /// batch after this one closes it: either as it is, or, with this function again, as
    /// the first page of that batch, which is then the same page with more on it and is
    /// painted as that batch has it. [`Session::close_open_page`] closes it as it is.
    pub fn add_pages_keeping_open(
        &mut self,
        document: &Document,
        later: &[usize],
    ) -> Result<(), Error> {
        self.add(document, later, true)
    }

    /// Writes what the page that was kept open paints, as it is. Does nothing without
    /// such a page.
    pub fn close_open_page(&mut self) -> Result<(), Error> {
        match self.open.take() {
            Some(open) => self.close(open),
            None => Ok(()),
        }
    }

    /// Whether the last page is kept open.
    pub fn has_open_page(&self) -> bool {
        self.open.is_some()
    }

    /// Writes what a page that was kept open paints, and what it links to.
    fn close(&mut self, open: OpenPage) -> Result<(), Error> {
        self.write_streams(vec![(open.ids.content, open.content)], 1)?;
        let link_ids: Vec<Ref> = open.links.iter().map(|_| self.refs.next()).collect();
        self.sink.object(|chunk| {
            chunk
                .indirect(open.ids.annotations)
                .array()
                .items(link_ids.iter().copied());
        })?;
        self.write_links(&open.batch, &open.links, open.height, &link_ids)?;
        match open.ids.later {
            Some(content_id) => {
                let later = LaterPage {
                    content_id,
                    resources_id: open.ids.resources,
                    used: open.used,
                    height: open.height,
                };
                if open.later {
                    self.later.insert(open.index, later);
                } else {
                    self.write_later(later, &[])?;
                }
            }
            None if open.later => return Err(Error::NothingLater),
            None => self.sink.object(|chunk| {
                let mut resources = chunk
                    .indirect(open.ids.resources)
                    .start::<pdf_writer::writers::Resources>();
                write_resources(
                    &mut resources,
                    &open.used,
                    &self.fonts,
                    &self.alphas,
                    &self.images,
                    &self.palette,
                );
            })?,
        }
        self.outline.extend(open.bookmarks);
        Ok(())
    }

    fn add(&mut self, document: &Document, later: &[usize], keep: bool) -> Result<(), Error> {
        let result = self.write_pages(document, later, keep);
        // The images of the batch may be gone from memory after it.
        self.batch_images.clear();
        result
    }

    /// Paints `items` on the page at `page`, counted in the file from zero, over what it
    /// has. The page is one that [`Session::add_pages_painted_later`] left open, and this
    /// closes it: another call for the same page paints nothing.
    pub fn paint_later(&mut self, page: usize, items: &[DisplayItem]) -> Result<(), Error> {
        let Some(later) = self.later.remove(&page) else {
            return Ok(());
        };
        let result = self.write_later(later, items);
        self.batch_images.clear();
        result
    }

    fn write_later(&mut self, later: LaterPage, items: &[DisplayItem]) -> Result<(), Error> {
        let mut content = Content::new();
        let mut used = later.used;
        self.render(&mut content, &mut used, items, later.height)?;
        self.write_streams(vec![(later.content_id, content.finish().into_vec())], 1)?;
        self.sink.object(|chunk| {
            let mut resources = chunk
                .indirect(later.resources_id)
                .start::<pdf_writer::writers::Resources>();
            write_resources(
                &mut resources,
                &used,
                &self.fonts,
                &self.alphas,
                &self.images,
                &self.palette,
            );
        })?;
        self.write_images()
    }

    fn write_pages(
        &mut self,
        document: &Document,
        later: &[usize],
        keep: bool,
    ) -> Result<(), Error> {
        // With `keep`, a page that was kept open is the first page of this batch, with
        // more on it. Otherwise it is closed as it is.
        let mut replaced = None;
        if let Some(open) = self.open.take() {
            if keep && !document.pages.is_empty() {
                replaced = Some(open);
            } else {
                self.close(open)?;
            }
        }
        let last = document.pages.len().saturating_sub(1);
        // Links can point to a later page of the batch, so its pages take their IDs first.
        let pages = document.pages.iter().enumerate();
        let batch: Vec<(Ref, f32)> = pages
            .map(|(index, page)| {
                let kept = replaced.as_ref().filter(|_| index == 0);
                let id = kept.map(|open| open.ids.page);
                (id.unwrap_or_else(|| self.refs.next()), page.size.height)
            })
            .collect();
        // Content streams are compressed in parallel, a batch of pages at a time, and written
        // after their pages under the IDs taken in page order.
        let threads = compression_threads();
        let mut pending: Vec<(Ref, Vec<u8>)> = Vec::new();
        for (index, (page, &(page_id, _))) in document.pages.iter().zip(&batch).enumerate() {
            rustyprint_dom::budget::check_deadline();
            if pending.len() >= threads * PAGES_PER_THREAD {
                self.write_streams(std::mem::take(&mut pending), threads)?;
            }
            let width = page.size.width * PX_TO_PT;
            let height = page.size.height * PX_TO_PT;

            let mut content = Content::new();
            let bleed = page.bleed * PX_TO_PT;
            let marked = page.marks.crop || page.marks.cross;
            if marked {
                marks(&mut content, page.marks, width, height, bleed);
            }
            let mut used = Resources::default();
            self.render(&mut content, &mut used, &page.items, height)?;

            // Content is drawn from the trim box's corner; the media box grows around it for
            // the bleed and the marks.
            let extent = bleed + if marked { MARKS_AREA } else { 0.0 };

            // A page that is kept open, and one that was: its page object says where what
            // it paints will be, which is written when the page is closed.
            let kept = replaced.take_if(|_| index == 0);
            let stays = keep && index == last;
            if kept.is_some() || stays {
                let (ids, at) = match &kept {
                    Some(open) => (open.ids, open.index),
                    None => {
                        let ids = OpenIds {
                            page: page_id,
                            content: self.refs.next(),
                            later: later.contains(&index).then(|| self.refs.next()),
                            resources: self.refs.next(),
                            annotations: self.refs.next(),
                        };
                        self.sink.object(|chunk| {
                            let mut page_writer = chunk.page(page_id);
                            page_writer
                                .parent(self.page_tree_id)
                                .media_box(Rect::new(
                                    -extent,
                                    -extent,
                                    width + extent,
                                    height + extent,
                                ))
                                .contents_array(std::iter::once(ids.content).chain(ids.later));
                            if extent > 0.0 {
                                page_writer
                                    .bleed_box(Rect::new(
                                        -bleed,
                                        -bleed,
                                        width + bleed,
                                        height + bleed,
                                    ))
                                    .trim_box(Rect::new(0.0, 0.0, width, height));
                            }
                            if page.rotate != 0 {
                                page_writer.rotate(page.rotate);
                            }
                            page_writer.pair(Name(b"Annots"), ids.annotations);
                            page_writer.pair(Name(b"Resources"), ids.resources);
                        })?;
                        self.page_ids.push(page_id);
                        (ids, self.page_ids.len() - 1)
                    }
                };
                self.write_images()?;
                let open = OpenPage {
                    ids,
                    index: at,
                    height,
                    content: content.finish().into_vec(),
                    used,
                    links: page.links.clone(),
                    batch: batch.clone(),
                    later: later.contains(&index),
                    bookmarks: Vec::new(),
                };
                if stays {
                    self.open = Some(open);
                } else {
                    self.close(open)?;
                }
                continue;
            }

            let content_id = self.refs.next();
            pending.push((content_id, content.finish().into_vec()));
            let link_ids: Vec<Ref> = page.links.iter().map(|_| self.refs.next()).collect();
            // A page that is painted on later has a second stream, and its resources are
            // an object of their own, written with that stream.
            let open = later
                .contains(&index)
                .then(|| (self.refs.next(), self.refs.next()));
            self.sink.object(|chunk| {
                let mut page_writer = chunk.page(page_id);
                // The contents are an array, so that a page can get more streams than one.
                page_writer
                    .parent(self.page_tree_id)
                    .media_box(Rect::new(-extent, -extent, width + extent, height + extent))
                    .contents_array(std::iter::once(content_id).chain(open.map(|(id, _)| id)));
                if extent > 0.0 {
                    page_writer
                        .bleed_box(Rect::new(-bleed, -bleed, width + bleed, height + bleed))
                        .trim_box(Rect::new(0.0, 0.0, width, height));
                }
                if page.rotate != 0 {
                    page_writer.rotate(page.rotate);
                }
                if !link_ids.is_empty() {
                    page_writer.annotations(link_ids.iter().copied());
                }
                match open {
                    Some((_, resources_id)) => {
                        page_writer.pair(Name(b"Resources"), resources_id);
                    }
                    None => write_resources(
                        &mut page_writer.resources(),
                        &used,
                        &self.fonts,
                        &self.alphas,
                        &self.images,
                        &self.palette,
                    ),
                }
            })?;
            self.write_links(&batch, &page.links, height, &link_ids)?;
            self.write_images()?;
            if let Some((content_id, resources_id)) = open {
                let later = LaterPage {
                    content_id,
                    resources_id,
                    used,
                    height,
                };
                self.later.insert(self.page_ids.len(), later);
            }
            self.page_ids.push(page_id);
        }
        self.write_streams(pending, threads)?;

        if !batch.is_empty() {
            for bookmark in &document.bookmarks {
                let (page, top) = target(&batch, &bookmark.target);
                let entry = OutlineEntry {
                    level: bookmark.level,
                    title: bookmark.title.clone(),
                    page,
                    top,
                };
                // The bookmarks to a page that is kept open are those of the page as it
                // is closed.
                match &mut self.open {
                    Some(open) if open.ids.page == page => open.bookmarks.push(entry),
                    _ => self.outline.push(entry),
                }
            }
        }
        Ok(())
    }

    /// Writes what the whole file shares (the outline, the fonts, the page tree and the
    /// catalog) and the cross-reference table, and returns the writer. A page that was to
    /// be painted on later and was not is closed as it is.
    pub fn finish(mut self) -> Result<W, Error> {
        self.close_open_page()?;
        // A page that was to be painted on later and was not has nothing more on it.
        let mut open: Vec<usize> = self.later.keys().copied().collect();
        open.sort_unstable();
        for page in open {
            self.paint_later(page, &[])?;
        }

        let outline_id = if self.outline.is_empty() {
            None
        } else {
            let id = self.refs.next();
            self.write_outline(id)?;
            Some(id)
        };

        for font in &self.fonts {
            font.write(&mut self.sink, &mut self.refs)?;
        }

        self.sink.object(|chunk| {
            chunk
                .pages(self.page_tree_id)
                .kids(self.page_ids.iter().copied())
                .count(self.page_ids.len() as i32);
        })?;
        self.sink.object(|chunk| {
            let mut catalog = chunk.indirect(self.catalog_id).start::<Catalog>();
            catalog.pages(self.page_tree_id);
            if let Some(outline_id) = outline_id {
                catalog.outlines(outline_id);
            }
        })?;
        Ok(self.sink.finish(self.catalog_id, self.info_id)?)
    }

    /// Compresses content streams and writes them under their IDs.
    fn write_streams(&mut self, pending: Vec<(Ref, Vec<u8>)>, threads: usize) -> Result<(), Error> {
        let (ids, streams): (Vec<Ref>, Vec<Vec<u8>>) = pending.into_iter().unzip();
        for (id, data) in ids.into_iter().zip(compress_all(streams, threads)) {
            self.sink.object(|chunk| {
                chunk.stream(id, &data).filter(Filter::FlateDecode);
            })?;
        }
        Ok(())
    }

    /// Writes the link annotations of a page `height` points high, whose `links` point to
    /// the pages of `batch`.
    fn write_links(
        &mut self,
        batch: &[(Ref, f32)],
        links: &[Link],
        height: f32,
        ids: &[Ref],
    ) -> Result<(), Error> {
        for (link, &id) in links.iter().zip(ids) {
            let rect = link.rect;
            self.sink.object(|chunk| {
                let mut annotation = chunk.annotation(id);
                annotation
                    .subtype(AnnotationType::Link)
                    .rect(Rect::new(
                        rect.x * PX_TO_PT,
                        height - rect.bottom() * PX_TO_PT,
                        rect.right() * PX_TO_PT,
                        height - rect.y * PX_TO_PT,
                    ))
                    .border(0.0, 0.0, 0.0, None);
                match &link.target {
                    LinkTarget::Uri(uri) => {
                        annotation
                            .action()
                            .action_type(ActionType::Uri)
                            .uri(Str(uri.as_bytes()));
                    }
                    LinkTarget::Internal(destination) => {
                        let (page, top) = target(batch, destination);
                        annotation
                            .action()
                            .action_type(ActionType::GoTo)
                            .destination()
                            .page(page)
                            .xyz(0.0, top, None);
                    }
                }
            })?;
        }
        Ok(())
    }

    /// Writes the images that no page before used, and lets go of their pixels.
    fn write_images(&mut self) -> Result<(), Error> {
        for index in self.images_written..self.images.len() {
            if let Some(image) = self.images[index].pending.take() {
                let (id, smooth) = (self.images[index].id, self.images[index].smooth);
                self.write_image(&image, id, smooth)?;
            }
        }
        self.images_written = self.images.len();
        Ok(())
    }

    /// Writes display items into `content`, noting the resources they use in `res`. Groups
    /// become transparency group XObjects with content streams of their own.
    fn render(
        &mut self,
        content: &mut Content,
        res: &mut Resources,
        items: &[DisplayItem],
        height: f32,
    ) -> Result<(), Error> {
        let mut index = 0;
        while index < items.len() {
            let item = &items[index];
            index += 1;
            if let DisplayItem::PushGroup(group) = item {
                let end = group_end(items, index);
                self.group(content, res, group, &items[index..end], height)?;
                index = (end + 1).min(items.len());
                continue;
            }
            let alpha = item_alpha(item);
            if alpha < 255 {
                content.save_state();
                content.set_parameters(Name(alpha_name(alpha).as_bytes()));
                if !res.alphas.contains(&alpha) {
                    res.alphas.push(alpha);
                }
                if !self.alphas.iter().any(|(a, _)| *a == alpha) {
                    let id = self.refs.next();
                    self.alphas.push((alpha, id));
                    let value = f32::from(alpha) / 255.0;
                    self.sink.object(|chunk| {
                        chunk
                            .ext_graphics(id)
                            .non_stroking_alpha(value)
                            .stroking_alpha(value);
                    })?;
                }
            }
            match item {
                DisplayItem::Text(run) => {
                    let font = self.text(content, height, run);
                    if !res.fonts.contains(&font) {
                        res.fonts.push(font);
                    }
                }
                DisplayItem::Rect { rect, color } => {
                    set_fill(content, *color, &self.palette);
                    content.rect(
                        rect.x * PX_TO_PT,
                        height - rect.bottom() * PX_TO_PT,
                        rect.width * PX_TO_PT,
                        rect.height * PX_TO_PT,
                    );
                    content.fill_nonzero();
                }
                DisplayItem::Polygon { points, color } => {
                    set_fill(content, *color, &self.palette);
                    path(content, points, height);
                    content.fill_nonzero();
                }
                DisplayItem::Polyline {
                    points,
                    width,
                    color,
                    closed,
                } => {
                    content.save_state();
                    set_stroke(content, *color, &self.palette);
                    content.set_line_width(width * PX_TO_PT);
                    content.set_line_join(LineJoinStyle::RoundJoin);
                    let mut pts = points
                        .iter()
                        .map(|p| (p.x * PX_TO_PT, height - p.y * PX_TO_PT));
                    if let Some((x, y)) = pts.next() {
                        content.move_to(x, y);
                    }
                    for (x, y) in pts {
                        content.line_to(x, y);
                    }
                    if *closed {
                        content.close_path();
                    }
                    content.stroke();
                    content.restore_state();
                }
                DisplayItem::Line {
                    from,
                    to,
                    width,
                    color,
                    style,
                } => {
                    content.save_state();
                    set_stroke(content, *color, &self.palette);
                    content.set_line_width(width * PX_TO_PT);
                    let w = width * PX_TO_PT;
                    // Fit the pattern to the line so it starts and ends with a dash or dot.
                    let length = (to.x - from.x).hypot(to.y - from.y) * PX_TO_PT;
                    match style {
                        LineStyle::Solid => {}
                        LineStyle::Dashed => {
                            let dashes = ((length / (3.0 * w) + 1.0) / 2.0).round().max(1.0);
                            let dash = length / (2.0 * dashes - 1.0);
                            content.set_dash_pattern([dash, dash], 0.0);
                        }
                        LineStyle::Dotted => {
                            let dots = (length / (2.0 * w)).round().max(1.0) + 1.0;
                            content.set_line_cap(LineCapStyle::RoundCap);
                            content.set_dash_pattern([0.0, length / (dots - 1.0)], 0.0);
                        }
                    }
                    path(content, &[*from, *to], height);
                    content.stroke();
                    content.restore_state();
                }
                DisplayItem::PushClip(rect) => {
                    content.save_state();
                    content.rect(
                        rect.x * PX_TO_PT,
                        height - rect.bottom() * PX_TO_PT,
                        rect.width * PX_TO_PT,
                        rect.height * PX_TO_PT,
                    );
                    content.clip_nonzero();
                    content.end_path();
                }
                DisplayItem::Path { path, color, rule } => {
                    set_fill(content, *color, &self.palette);
                    curves(content, path, height);
                    match rule {
                        FillRule::NonZero => content.fill_nonzero(),
                        FillRule::EvenOdd => content.fill_even_odd(),
                    };
                }
                DisplayItem::StrokePath {
                    path,
                    width,
                    color,
                    style,
                } => {
                    content.save_state();
                    set_stroke(content, *color, &self.palette);
                    let w = width * PX_TO_PT;
                    content.set_line_width(w);
                    // Fit a whole number of dashes or dots around the path.
                    let length = path.length() * PX_TO_PT;
                    let period = |target: f32| length / (length / target).round().max(1.0);
                    match style {
                        LineStyle::Solid => {}
                        LineStyle::Dashed => {
                            let dash = period(6.0 * w) / 2.0;
                            content.set_dash_pattern([dash, dash], 0.0);
                        }
                        LineStyle::Dotted => {
                            content.set_line_cap(LineCapStyle::RoundCap);
                            content.set_dash_pattern([0.0, period(2.0 * w)], 0.0);
                        }
                    }
                    curves(content, path, height);
                    content.stroke();
                    content.restore_state();
                }
                DisplayItem::PushClipPath { path, rule } => {
                    content.save_state();
                    curves(content, path, height);
                    match rule {
                        FillRule::NonZero => content.clip_nonzero(),
                        FillRule::EvenOdd => content.clip_even_odd(),
                    };
                    content.end_path();
                }
                DisplayItem::Image {
                    image,
                    rect,
                    smooth,
                    orientation,
                    ..
                } => {
                    let index = self.image_index(image, *smooth);
                    if !res.images.contains(&index) {
                        res.images.push(index);
                    }
                    content.save_state();
                    content.transform(image_matrix(
                        *orientation,
                        rect.x * PX_TO_PT,
                        height - rect.bottom() * PX_TO_PT,
                        rect.width * PX_TO_PT,
                        rect.height * PX_TO_PT,
                    ));
                    content.x_object(Name(image_name(index).as_bytes()));
                    content.restore_state();
                }
                DisplayItem::Gradient { rect, shading } => {
                    let resource = self.shading(shading, height)?;
                    content.save_state();
                    content.rect(
                        rect.x * PX_TO_PT,
                        height - rect.bottom() * PX_TO_PT,
                        rect.width * PX_TO_PT,
                        rect.height * PX_TO_PT,
                    );
                    content.clip_nonzero();
                    content.end_path();
                    if let Some((name, id)) = resource.mask {
                        content.set_parameters(Name(name.as_bytes()));
                        res.states.push((name, id));
                    }
                    if let Some(matrix) = ellipse_matrix(shading, height) {
                        content.transform(matrix);
                    }
                    content.shading(Name(resource.name.as_bytes()));
                    content.restore_state();
                    res.shadings.push((resource.name, resource.id));
                }
                DisplayItem::PushClipText(runs) => {
                    for font in self.clip_text(content, height, runs) {
                        if !res.fonts.contains(&font) {
                            res.fonts.push(font);
                        }
                    }
                }
                DisplayItem::PushTransform([a, b, c, d, e, f]) => {
                    // The matrix in PDF space, whose y axis points up from the bottom.
                    content.save_state();
                    content.transform([
                        *a,
                        -b,
                        -c,
                        *d,
                        c * height + e * PX_TO_PT,
                        height * (1.0 - d) - f * PX_TO_PT,
                    ]);
                }
                DisplayItem::PopClip => {
                    content.restore_state();
                }
                // Groups are written above; a group without a start, or a backdrop layout did
                // not resolve, paints nothing.
                DisplayItem::PushGroup(_) | DisplayItem::PopGroup | DisplayItem::Backdrop(_) => {}
            }
            if alpha < 255 {
                content.restore_state();
            }
        }
        Ok(())
    }

    /// Writes the items of a group as a transparency group XObject and paints it with the
    /// group's opacity, blend mode and mask.
    fn group(
        &mut self,
        content: &mut Content,
        res: &mut Resources,
        group: &Group,
        items: &[DisplayItem],
        height: f32,
    ) -> Result<(), Error> {
        let mut inner = Content::new();
        let mut used = Resources::default();
        self.render(&mut inner, &mut used, items, height)?;
        let data = compress_to_vec_zlib(&inner.finish(), COMPRESSION_LEVEL);
        let form_id = self.refs.next();
        self.sink.object(|chunk| {
            let mut form = chunk.form_xobject(form_id, &data);
            form.filter(Filter::FlateDecode);
            form.bbox(Rect::new(-1.0e5, -1.0e5, 1.0e5, 1.0e5));
            form.group()
                .transparency()
                .isolated(group.isolated)
                .knockout(false)
                .color_space()
                .device_rgb();
            write_resources(
                &mut form.resources(),
                &used,
                &self.fonts,
                &self.alphas,
                &self.images,
                &self.palette,
            );
        })?;

        let mut mask_form = None;
        if let Some(mask) = &group.mask {
            let index = self.image_index(&mask.image, true);
            let (image_name, image_id) = (image_name(index), self.images[index].id);
            let r = mask.rect;
            let mut mask_content = Content::new();
            mask_content.save_state();
            mask_content.transform([
                r.width * PX_TO_PT,
                0.0,
                0.0,
                r.height * PX_TO_PT,
                r.x * PX_TO_PT,
                height - r.bottom() * PX_TO_PT,
            ]);
            mask_content.x_object(Name(image_name.as_bytes()));
            mask_content.restore_state();
            let data = mask_content.finish();
            let mask_id = self.refs.next();
            self.sink.object(|chunk| {
                let mut form = chunk.form_xobject(mask_id, &data);
                form.bbox(Rect::new(-1.0e5, -1.0e5, 1.0e5, 1.0e5));
                form.group().transparency().color_space().device_gray();
                form.resources()
                    .x_objects()
                    .pair(Name(image_name.as_bytes()), image_id);
            })?;
            mask_form = Some(mask_id);
        }

        let state_id = self.refs.next();
        self.sink.object(|chunk| {
            let mut state = chunk.ext_graphics(state_id);
            state
                .non_stroking_alpha(group.opacity)
                .stroking_alpha(group.opacity)
                .blend_mode(blend_mode(group.blend));
            if let Some(mask_id) = mask_form {
                state
                    .soft_mask()
                    .subtype(MaskType::Luminosity)
                    .group(mask_id);
            }
        })?;

        self.groups += 1;
        let state_name = format!("GS{}", self.groups);
        let form_name = format!("Fm{}", self.groups);
        content.save_state();
        content.set_parameters(Name(state_name.as_bytes()));
        content.x_object(Name(form_name.as_bytes()));
        content.restore_state();
        res.states.push((state_name, state_id));
        res.forms.push((form_name, form_id));
        Ok(())
    }

    /// Writes the document outline: each bookmark nests under the closest one before it with
    /// a lower level.
    fn write_outline(&mut self, outline_id: Ref) -> Result<(), Error> {
        let bookmarks = std::mem::take(&mut self.outline);
        let ids: Vec<Ref> = bookmarks.iter().map(|_| self.refs.next()).collect();
        let mut parents: Vec<Option<usize>> = Vec::with_capacity(bookmarks.len());
        let mut open: Vec<usize> = Vec::new();
        for (index, bookmark) in bookmarks.iter().enumerate() {
            while open
                .last()
                .is_some_and(|&last| bookmarks[last].level >= bookmark.level)
            {
                open.pop();
            }
            parents.push(open.last().copied());
            open.push(index);
        }
        // The children of each bookmark, and the top-level bookmarks last.
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); bookmarks.len() + 1];
        for (index, parent) in parents.iter().enumerate() {
            children[parent.unwrap_or(bookmarks.len())].push(index);
        }
        let mut descendants = vec![0; bookmarks.len()];
        for index in (0..bookmarks.len()).rev() {
            if let Some(parent) = parents[index] {
                descendants[parent] += 1 + descendants[index];
            }
        }

        let top = &children[bookmarks.len()];
        self.sink.object(|chunk| {
            chunk
                .outline(outline_id)
                .first(ids[top[0]])
                .last(ids[top[top.len() - 1]])
                .count(bookmarks.len() as i32);
        })?;
        for (index, bookmark) in bookmarks.iter().enumerate() {
            let siblings = &children[parents[index].unwrap_or(bookmarks.len())];
            let position = siblings
                .iter()
                .position(|&i| i == index)
                .expect("a bookmark is among its parent's children");
            self.sink.object(|chunk| {
                let mut item = chunk.outline_item(ids[index]);
                item.title(TextStr(&bookmark.title))
                    .parent(parents[index].map_or(outline_id, |p| ids[p]));
                if position > 0 {
                    item.prev(ids[siblings[position - 1]]);
                }
                if let Some(&next) = siblings.get(position + 1) {
                    item.next(ids[next]);
                }
                let kids = &children[index];
                if let (Some(&first), Some(&last)) = (kids.first(), kids.last()) {
                    item.first(ids[first])
                        .last(ids[last])
                        .count(descendants[index]);
                }
                item.dest().page(bookmark.page).xyz(0.0, bookmark.top, None);
            })?;
        }
        Ok(())
    }

    /// Writes a shading, and for stops that are not all opaque a graphics state whose soft
    /// mask is a gray shading of their alpha.
    fn shading(&mut self, shading: &Shading, page_height: f32) -> Result<ShadingResource, Error> {
        let index = self.shadings;
        self.shadings += 1;
        let color: Vec<(f32, Vec<f32>)> = shading
            .stops
            .iter()
            .map(|(t, c)| (*t, vec![channel(c.r), channel(c.g), channel(c.b)]))
            .collect();
        let id = self.shading_object(shading, &color, page_height)?;
        let mask = if shading.stops.iter().all(|(_, c)| c.a == 255) {
            None
        } else {
            let alpha: Vec<(f32, Vec<f32>)> = shading
                .stops
                .iter()
                .map(|(t, c)| (*t, vec![channel(c.a)]))
                .collect();
            let mask_shading = self.shading_object(shading, &alpha, page_height)?;
            let mut content = Content::new();
            if let Some(matrix) = ellipse_matrix(shading, page_height) {
                content.transform(matrix);
            }
            content.shading(Name(b"Sh"));
            let data = content.finish();
            let form_id = self.refs.next();
            self.sink.object(|chunk| {
                let mut form = chunk.form_xobject(form_id, &data);
                form.bbox(Rect::new(-1.0e5, -1.0e5, 1.0e5, 1.0e5));
                form.group().transparency().color_space().device_gray();
                form.resources().shadings().pair(Name(b"Sh"), mask_shading);
            })?;
            let state_id = self.refs.next();
            self.sink.object(|chunk| {
                chunk
                    .ext_graphics(state_id)
                    .soft_mask()
                    .subtype(MaskType::Luminosity)
                    .group(form_id);
            })?;
            Some((format!("M{index}"), state_id))
        };
        Ok(ShadingResource {
            name: format!("Sh{index}"),
            id,
            mask,
        })
    }

    /// Writes a shading dictionary whose function gives `values` at its stops: three RGB
    /// components, or one gray component.
    fn shading_object(
        &mut self,
        shading: &Shading,
        values: &[(f32, Vec<f32>)],
        page_height: f32,
    ) -> Result<Ref, Error> {
        let function = self.stops_function(values)?;
        let id = self.refs.next();
        self.sink.object(|chunk| {
            let mut dict = chunk.function_shading(id);
            match shading.kind {
                ShadingKind::Axial { start, end } => {
                    dict.shading_type(FunctionShadingType::Axial).coords([
                        start.x * PX_TO_PT,
                        page_height - start.y * PX_TO_PT,
                        end.x * PX_TO_PT,
                        page_height - end.y * PX_TO_PT,
                    ]);
                }
                ShadingKind::Radial { center, r0, r1, .. } => {
                    let (x, y) = (center.x * PX_TO_PT, page_height - center.y * PX_TO_PT);
                    dict.shading_type(FunctionShadingType::Radial).coords([
                        x,
                        y,
                        r0 * PX_TO_PT,
                        x,
                        y,
                        r1 * PX_TO_PT,
                    ]);
                }
            }
            if values.first().is_some_and(|(_, v)| v.len() == 1) {
                dict.color_space().device_gray();
            } else {
                dict.color_space().device_rgb();
            }
            dict.function(function).extend([true, true]);
        })?;
        Ok(id)
    }

    /// A function over [0, 1] that interpolates linearly between the values at the stops.
    fn stops_function(&mut self, stops: &[(f32, Vec<f32>)]) -> Result<Ref, Error> {
        let mut stops = stops.to_vec();
        if stops.len() == 1 {
            stops.push((1.0, stops[0].1.clone()));
        }
        // Stitching bounds must increase; stops at the same place become a sharp step.
        for i in 1..stops.len() {
            if stops[i].0 <= stops[i - 1].0 {
                stops[i].0 = stops[i - 1].0 + 1.0e-5;
            }
        }
        let mut segments: Vec<Ref> = Vec::with_capacity(stops.len());
        for pair in stops.windows(2) {
            let id = self.refs.next();
            self.sink.object(|chunk| {
                chunk
                    .exponential_function(id)
                    .domain([0.0, 1.0])
                    .c0(pair[0].1.iter().copied())
                    .c1(pair[1].1.iter().copied())
                    .n(1.0);
            })?;
            segments.push(id);
        }
        if segments.len() == 1 {
            return Ok(segments[0]);
        }
        let last = stops.last().map_or(1.0, |s| s.0.max(1.0));
        let id = self.refs.next();
        self.sink.object(|chunk| {
            chunk
                .stitching_function(id)
                .domain([0.0, last])
                .functions(segments.iter().copied())
                .bounds(stops[1..stops.len() - 1].iter().map(|s| s.0))
                .encode(segments.iter().flat_map(|_| [0.0, 1.0]));
        })?;
        Ok(id)
    }

    /// Returns the index of `font` in `self.fonts`, adding it on first use.
    fn font_index(&mut self, font: &Font) -> usize {
        if let Some(index) = self
            .fonts
            .iter()
            .position(|resource| resource.font == *font)
        {
            return index;
        }
        let type0_id = self.refs.next();
        let name = format!("F{}", self.fonts.len());
        self.fonts
            .push(FontResource::new(font.clone(), name, type0_id));
        self.fonts.len() - 1
    }

    /// The glyphs of a run as `TJ` items, and the index of the run's font.
    fn positioned(&mut self, run: &TextRun) -> (usize, Vec<Positioned>) {
        let index = self.font_index(&run.font);
        let resource = &mut self.fonts[index];

        // Glyph positions are written in thousandths of an em, the unit of `TJ` adjustments.
        let to_em = 1000.0 / (run.size * run.font.size_adjust());
        let mut items = Vec::new();
        // Text rise outlasts the text object, so every run sets it.
        let mut rise = f32::NAN;
        let mut start = 0;
        while start < run.glyphs.len() {
            let range = run.glyphs[start].text.clone();
            let end = start
                + run.glyphs[start..]
                    .iter()
                    .take_while(|g| g.text == range)
                    .count();
            let cluster = &run.glyphs[start..end];
            let text = &run.text[range];
            // The cluster's text goes to its glyph with the widest advance: marks, such as
            // Arabic dots, have none and are shared between letters.
            let base = (0..cluster.len())
                .rev()
                .max_by(|&a, &b| {
                    let advance = |i: usize| cluster[i].x_advance.abs();
                    advance(a).total_cmp(&advance(b))
                })
                .expect("clusters are not empty");
            let mut mapped = cluster.len() == 1;
            let cluster_start = items.len();
            for (i, glyph) in cluster.iter().enumerate() {
                let glyph_rise = -glyph.y_offset * PX_TO_PT;
                if glyph_rise != rise {
                    rise = glyph_rise;
                    items.push(Positioned::Rise(rise));
                }
                let (cid, maps_to_text) =
                    resource.use_glyph(glyph.id, if i == base { text } else { "" });
                mapped &= maps_to_text;
                let width = resource.width(glyph.id);
                let x_offset = glyph.x_offset * to_em;
                let advance = glyph.x_advance * to_em;
                if x_offset != 0.0 {
                    items.push(Positioned::Adjust(-x_offset));
                }
                items.push(Positioned::Glyph(cid));
                let adjust = width - advance + x_offset;
                if adjust.abs() > 1e-3 {
                    items.push(Positioned::Adjust(adjust));
                }
            }
            // Text the font's ToUnicode map cannot give is copied from the content instead.
            if !mapped && !text.is_empty() {
                items.insert(cluster_start, Positioned::ActualText(text.to_owned()));
                items.push(Positioned::EndActualText);
            }
            start = end;
        }
        (index, items)
    }

    /// Paints a text run and returns the index of the font it used.
    fn text(&mut self, content: &mut Content, page_height: f32, run: &TextRun) -> usize {
        let (index, items) = self.positioned(run);
        let name = self.fonts[index].name.clone();

        // Which passes paint the glyphs: a stroke painted first needs its own pass.
        let passes: Vec<(TextRenderingMode, Option<(Color, f32)>)> = match run.stroke {
            Some(stroke) if run.stroke_first && run.fill => vec![
                (
                    TextRenderingMode::Stroke,
                    Some((stroke.color, stroke.width)),
                ),
                (TextRenderingMode::Fill, None),
            ],
            Some(stroke) if run.fill => vec![(
                TextRenderingMode::FillStroke,
                Some((stroke.color, stroke.width)),
            )],
            Some(stroke) => vec![(
                TextRenderingMode::Stroke,
                Some((stroke.color, stroke.width)),
            )],
            // Synthesized bold strokes the outline in the fill color.
            None if run.embolden > 0.0 => vec![(
                TextRenderingMode::FillStroke,
                Some((run.color, run.embolden)),
            )],
            // Text over color glyphs is only there to be searched and copied.
            None if !run.fill => vec![(TextRenderingMode::Invisible, None)],
            None => vec![(TextRenderingMode::Fill, None)],
        };
        for (mode, stroke) in passes {
            content.begin_text();
            set_fill(content, run.color, &self.palette);
            if let Some((color, width)) = stroke {
                set_stroke(content, color, &self.palette);
                content.set_line_width(width * PX_TO_PT);
            }
            if mode != TextRenderingMode::Fill {
                content.set_text_rendering_mode(mode);
            }
            show(content, &name, run, page_height, &items);
            if mode != TextRenderingMode::Fill {
                content.set_text_rendering_mode(TextRenderingMode::Fill);
            }
            content.end_text();
        }
        index
    }

    /// Starts clipping to the glyphs of `runs`, which share one text object so that their
    /// outlines add up to one clipping path. Returns the indices of the fonts used.
    fn clip_text(
        &mut self,
        content: &mut Content,
        page_height: f32,
        runs: &[TextRun],
    ) -> Vec<usize> {
        let mut fonts = Vec::new();
        content.save_state();
        content.begin_text();
        content.set_text_rendering_mode(TextRenderingMode::Clip);
        for run in runs {
            let (index, items) = self.positioned(run);
            let name = self.fonts[index].name.clone();
            show(content, &name, run, page_height, &items);
            if !fonts.contains(&index) {
                fonts.push(index);
            }
        }
        content.end_text();
        fonts
    }

    /// Returns the index of `image` in `self.images`, adding it on first use.
    fn image_index(&mut self, image: &Arc<Image>, smooth: bool) -> usize {
        let address = Arc::as_ptr(image) as usize;
        if let Some(&(_, _, index)) = self
            .batch_images
            .iter()
            .find(|&&(a, s, _)| a == address && s == smooth)
        {
            return index;
        }
        let key = ImageKey::of(image, smooth);
        let index = match self.image_keys.get(&key) {
            Some(&index) => index,
            None => {
                let id = self.refs.next();
                self.images.push(ImageResource {
                    pending: Some(image.clone()),
                    id,
                    smooth,
                });
                self.image_keys.insert(key, self.images.len() - 1);
                self.images.len() - 1
            }
        };
        self.batch_images.push((address, smooth, index));
        index
    }

    /// Writes an image XObject, with a soft mask for its alpha channel.
    fn write_image(&mut self, image: &Image, id: Ref, smooth: bool) -> Result<(), Error> {
        let (width, height) = (image.width as i32, image.height as i32);
        match &image.data {
            ImageData::Jpeg { data, components } => {
                self.sink.object(|chunk| {
                    let mut xobject = chunk.image_xobject(id, data);
                    xobject.filter(Filter::DctDecode);
                    xobject.width(width).height(height).bits_per_component(8);
                    xobject.interpolate(smooth);
                    if *components == 1 {
                        xobject.color_space().device_gray();
                    } else {
                        xobject.color_space().device_rgb();
                    }
                })?;
            }
            ImageData::Pixels {
                samples,
                gray,
                alpha,
            } => {
                let mask_id = alpha.as_ref().map(|_| self.refs.next());
                let data = compress_to_vec_zlib(samples, COMPRESSION_LEVEL);
                self.sink.object(|chunk| {
                    let mut xobject = chunk.image_xobject(id, &data);
                    xobject.filter(Filter::FlateDecode);
                    xobject.width(width).height(height).bits_per_component(8);
                    xobject.interpolate(smooth);
                    if *gray {
                        xobject.color_space().device_gray();
                    } else {
                        xobject.color_space().device_rgb();
                    }
                    if let Some(mask_id) = mask_id {
                        xobject.s_mask(mask_id);
                    }
                })?;
                drop(data);
                if let (Some(mask_id), Some(alpha)) = (mask_id, alpha) {
                    let data = compress_to_vec_zlib(alpha, COMPRESSION_LEVEL);
                    self.sink.object(|chunk| {
                        let mut xobject = chunk.image_xobject(mask_id, &data);
                        xobject.filter(Filter::FlateDecode);
                        xobject.width(width).height(height).bits_per_component(8);
                        xobject.color_space().device_gray();
                    })?;
                }
            }
        }
        Ok(())
    }
}

/// The index of the `PopGroup` that ends a group whose items start at `start`, or the end of
/// the items if the group is not closed.
fn group_end(items: &[DisplayItem], start: usize) -> usize {
    let mut depth = 0;
    for (index, item) in items.iter().enumerate().skip(start) {
        match item {
            DisplayItem::PushGroup(_) => depth += 1,
            DisplayItem::PopGroup if depth == 0 => return index,
            DisplayItem::PopGroup => depth -= 1,
            _ => {}
        }
    }
    items.len()
}

/// The PDF blend mode for a CSS one. PDF has no additive modes, so `plus-lighter` is drawn
/// as `screen` and `plus-darker` as `multiply`.
fn blend_mode(mode: rustyprint_layout::display::BlendMode) -> PdfBlendMode {
    use rustyprint_layout::display::BlendMode as B;
    match mode {
        B::Normal => PdfBlendMode::Normal,
        B::Multiply | B::PlusDarker => PdfBlendMode::Multiply,
        B::Screen | B::PlusLighter => PdfBlendMode::Screen,
        B::Overlay => PdfBlendMode::Overlay,
        B::Darken => PdfBlendMode::Darken,
        B::Lighten => PdfBlendMode::Lighten,
        B::ColorDodge => PdfBlendMode::ColorDodge,
        B::ColorBurn => PdfBlendMode::ColorBurn,
        B::HardLight => PdfBlendMode::HardLight,
        B::SoftLight => PdfBlendMode::SoftLight,
        B::Difference => PdfBlendMode::Difference,
        B::Exclusion => PdfBlendMode::Exclusion,
        B::Hue => PdfBlendMode::Hue,
        B::Saturation => PdfBlendMode::Saturation,
        B::Color => PdfBlendMode::Color,
        B::Luminosity => PdfBlendMode::Luminosity,
    }
}

/// The resources one content stream uses, in order of first use.
#[derive(Default)]
struct Resources {
    /// Indices into the writer's fonts.
    fonts: Vec<usize>,
    alphas: Vec<u8>,
    /// Indices into the writer's images.
    images: Vec<usize>,
    shadings: Vec<(String, Ref)>,
    /// Graphics states of shading masks and groups.
    states: Vec<(String, Ref)>,
    /// Group XObjects.
    forms: Vec<(String, Ref)>,
}

/// Lists the resources a content stream uses in its resource dictionary.
fn write_resources(
    resources: &mut pdf_writer::writers::Resources,
    used: &Resources,
    fonts: &[FontResource],
    alphas: &[(u8, Ref)],
    images: &[ImageResource],
    palette: &Palette,
) {
    if !palette.profiles.is_empty() {
        let mut spaces = resources.color_spaces();
        for (name, id, _) in &palette.profiles {
            let mut space = spaces.insert(Name(name.as_bytes())).array();
            space.item(Name(b"ICCBased"));
            space.item(*id);
        }
        spaces.finish();
    }
    let mut font_dict = resources.fonts();
    for &index in &used.fonts {
        let resource = &fonts[index];
        font_dict.pair(Name(resource.name.as_bytes()), resource.type0_id);
    }
    font_dict.finish();
    let mut states = resources.ext_g_states();
    for &alpha in &used.alphas {
        let (_, id) = alphas
            .iter()
            .find(|(a, _)| *a == alpha)
            .expect("registered when used");
        states.pair(Name(alpha_name(alpha).as_bytes()), *id);
    }
    for (name, id) in &used.states {
        states.pair(Name(name.as_bytes()), *id);
    }
    states.finish();
    if !used.shadings.is_empty() {
        let mut shadings = resources.shadings();
        for (name, id) in &used.shadings {
            shadings.pair(Name(name.as_bytes()), *id);
        }
        shadings.finish();
    }
    let mut x_objects = resources.x_objects();
    for &index in &used.images {
        x_objects.pair(Name(image_name(index).as_bytes()), images[index].id);
    }
    for (name, id) in &used.forms {
        x_objects.pair(Name(name.as_bytes()), *id);
    }
}

/// A shading written for one use, and the graphics state that masks its alpha.
struct ShadingResource {
    name: String,
    id: Ref,
    mask: Option<(String, Ref)>,
}

/// The matrix that stretches a radial shading's circles vertically into ellipses, about
/// their center.
fn ellipse_matrix(shading: &Shading, page_height: f32) -> Option<[f32; 6]> {
    match shading.kind {
        ShadingKind::Radial {
            center, scale_y, ..
        } if (scale_y - 1.0).abs() > 1.0e-4 => {
            let y = page_height - center.y * PX_TO_PT;
            Some([1.0, 0.0, 0.0, scale_y, 0.0, y * (1.0 - scale_y)])
        }
        _ => None,
    }
}

/// An image the file uses and its XObject.
struct ImageResource {
    /// The image, until it is written with the page that uses it first.
    pending: Option<Arc<Image>>,
    id: Ref,
    /// Whether viewers smooth the image when they scale it.
    smooth: bool,
}

/// The resource name of the image at `index` of the session's images.
fn image_name(index: usize) -> String {
    format!("Im{index}")
}

/// What makes two images the same XObject: their size, their kind and a hash of their bytes.
#[derive(PartialEq, Eq, Hash)]
struct ImageKey {
    width: u32,
    height: u32,
    smooth: bool,
    /// How many bytes the image and its alpha channel have.
    lengths: (usize, usize),
    /// Two hashes of the bytes, which start from different states.
    hashes: [u64; 2],
}

impl ImageKey {
    fn of(image: &Image, smooth: bool) -> Self {
        let (kind, bytes, alpha): (u8, &[u8], &[u8]) = match &image.data {
            ImageData::Jpeg { data, components } => (*components, data, &[]),
            ImageData::Pixels {
                samples,
                gray,
                alpha,
            } => (
                if *gray { 101 } else { 103 },
                samples,
                alpha.as_deref().unwrap_or(&[]),
            ),
        };
        let hash = |seed: u8| {
            let mut hasher = DefaultHasher::new();
            (seed, kind).hash(&mut hasher);
            bytes.hash(&mut hasher);
            alpha.hash(&mut hasher);
            Hasher::finish(&hasher)
        };
        Self {
            width: image.width,
            height: image.height,
            smooth,
            lengths: (bytes.len(), alpha.len()),
            hashes: [hash(0), hash(1)],
        }
    }
}

/// The matrix that draws an image's unit square into the rectangle at `(x, y)` of `w` × `h`
/// (bottom-left corner, in points), turned and flipped by an EXIF orientation. Image rows
/// run from the top, at `v = 1`.
fn image_matrix(orientation: u8, x: f32, y: f32, w: f32, h: f32) -> [f32; 6] {
    // Where the image's u and v axes go in the rectangle's unit square:
    // `X = a u + c v + e` and `Y = b u + d v + f`.
    let [a, b, c, d, e, f] = match orientation {
        2 => [-1.0, 0.0, 0.0, 1.0, 1.0, 0.0],
        3 => [-1.0, 0.0, 0.0, -1.0, 1.0, 1.0],
        4 => [1.0, 0.0, 0.0, -1.0, 0.0, 1.0],
        5 => [0.0, -1.0, -1.0, 0.0, 1.0, 1.0],
        6 => [0.0, -1.0, 1.0, 0.0, 0.0, 1.0],
        7 => [0.0, 1.0, 1.0, 0.0, 0.0, 0.0],
        8 => [0.0, 1.0, -1.0, 0.0, 1.0, 0.0],
        _ => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };
    [a * w, b * h, c * w, d * h, e * w + x, f * h + y]
}

/// Shows the positioned glyphs of `run` in the font resource `font`, inside a text object.
fn show(content: &mut Content, font: &str, run: &TextRun, page_height: f32, items: &[Positioned]) {
    let skew = run.oblique.to_radians().tan();
    content.set_font(
        Name(font.as_bytes()),
        run.size * run.font.size_adjust() * PX_TO_PT,
    );
    content.set_text_matrix([
        1.0,
        0.0,
        skew,
        1.0,
        run.origin.x * PX_TO_PT,
        page_height - run.origin.y * PX_TO_PT,
    ]);
    let mut rest = items;
    while let Some(item) = rest.first() {
        match item {
            Positioned::Rise(rise) => {
                content.set_rise(*rise);
            }
            Positioned::ActualText(text) => {
                content
                    .begin_marked_content_with_properties(Name(b"Span"))
                    .properties()
                    .actual_text(TextStr(text));
            }
            Positioned::EndActualText => {
                content.end_marked_content();
            }
            Positioned::Glyph(_) | Positioned::Adjust(_) => {
                let count = rest
                    .iter()
                    .take_while(|item| matches!(item, Positioned::Glyph(_) | Positioned::Adjust(_)))
                    .count();
                let mut show = content.show_positioned();
                let mut writer = show.items();
                for item in &rest[..count] {
                    match *item {
                        Positioned::Glyph(cid) => writer.show(Str(&cid.to_be_bytes())),
                        Positioned::Adjust(amount) => writer.adjust(amount),
                        _ => unreachable!("only glyphs and adjustments are counted"),
                    };
                }
                rest = &rest[count..];
                continue;
            }
        }
        rest = &rest[1..];
    }
}

/// The page and the top edge in PDF coordinates of a destination.
fn target(pages: &[(Ref, f32)], destination: &Destination) -> (Ref, f32) {
    let (page, height) = pages[destination.page.min(pages.len().saturating_sub(1))];
    (page, (height - destination.y) * PX_TO_PT)
}

fn channel(value: u8) -> f32 {
    f32::from(value) / 255.0
}

fn set_fill(content: &mut Content, color: Color, palette: &Palette) {
    match color.native.and_then(|n| palette.native(n)) {
        Some(NativeFill::Cmyk([c, m, y, k])) => {
            content.set_fill_cmyk(c, m, y, k);
        }
        Some(NativeFill::Icc(name, components, count)) => {
            content.set_fill_color_space(Name(name.as_bytes()));
            content.set_fill_color(components[..count].iter().copied());
        }
        None => {
            content.set_fill_rgb(channel(color.r), channel(color.g), channel(color.b));
        }
    }
}

fn set_stroke(content: &mut Content, color: Color, palette: &Palette) {
    match color.native.and_then(|n| palette.native(n)) {
        Some(NativeFill::Cmyk([c, m, y, k])) => {
            content.set_stroke_cmyk(c, m, y, k);
        }
        Some(NativeFill::Icc(name, components, count)) => {
            content.set_stroke_color_space(Name(name.as_bytes()));
            content.set_stroke_color(components[..count].iter().copied());
        }
        None => {
            content.set_stroke_rgb(channel(color.r), channel(color.g), channel(color.b));
        }
    }
}

/// A closed path through `points` (or an open one for two points), in PDF coordinates.
fn path(content: &mut Content, points: &[Point], page_height: f32) {
    let mut points = points
        .iter()
        .map(|p| (p.x * PX_TO_PT, page_height - p.y * PX_TO_PT));
    if let Some((x, y)) = points.next() {
        content.move_to(x, y);
    }
    let mut count = 1;
    for (x, y) in points {
        content.line_to(x, y);
        count += 1;
    }
    if count > 2 {
        content.close_path();
    }
}

/// Draws crop marks at the corners of the trim box and registration crosses beside the
/// middle of each side, outside the bleed. Sizes are in points.
fn marks(content: &mut Content, marks: PrintMarks, width: f32, height: f32, bleed: f32) {
    let gap = bleed + 3.0;
    let length = MARKS_AREA - 6.0;
    content.save_state();
    content.set_stroke_gray(0.0);
    content.set_line_width(0.5);
    fn line(content: &mut Content, x1: f32, y1: f32, x2: f32, y2: f32) {
        content.move_to(x1, y1);
        content.line_to(x2, y2);
    }
    if marks.crop {
        for (x, dx) in [(0.0, -1.0), (width, 1.0)] {
            for (y, dy) in [(0.0, -1.0), (height, 1.0)] {
                // One horizontal and one vertical line along each edge through the corner.
                line(content, x + dx * gap, y, x + dx * (gap + length), y);
                line(content, x, y + dy * gap, x, y + dy * (gap + length));
            }
        }
    }
    if marks.cross {
        let r = length / 4.0;
        let middle = gap + length / 2.0;
        for (cx, cy) in [
            (width / 2.0, height + middle),
            (width / 2.0, -middle),
            (-middle, height / 2.0),
            (width + middle, height / 2.0),
        ] {
            line(content, cx - 2.0 * r, cy, cx + 2.0 * r, cy);
            line(content, cx, cy - 2.0 * r, cx, cy + 2.0 * r);
            // A circle from four Bézier quarters.
            let k = 0.552_284_8 * r;
            content.move_to(cx + r, cy);
            content.cubic_to(cx + r, cy + k, cx + k, cy + r, cx, cy + r);
            content.cubic_to(cx - k, cy + r, cx - r, cy + k, cx - r, cy);
            content.cubic_to(cx - r, cy - k, cx - k, cy - r, cx, cy - r);
            content.cubic_to(cx + k, cy - r, cx + r, cy - k, cx + r, cy);
        }
    }
    content.stroke();
    content.restore_state();
}

/// Adds the contours of `path` to the current path, in PDF coordinates.
fn curves(content: &mut Content, path: &Path, page_height: f32) {
    let pt = |p: Point| (p.x * PX_TO_PT, page_height - p.y * PX_TO_PT);
    for segment in &path.segments {
        match *segment {
            PathSegment::MoveTo(p) => {
                let (x, y) = pt(p);
                content.move_to(x, y);
            }
            PathSegment::LineTo(p) => {
                let (x, y) = pt(p);
                content.line_to(x, y);
            }
            PathSegment::CubicTo(a, b, c) => {
                let ((x1, y1), (x2, y2), (x3, y3)) = (pt(a), pt(b), pt(c));
                content.cubic_to(x1, y1, x2, y2, x3, y3);
            }
            PathSegment::Close => {
                content.close_path();
            }
        }
    }
}

/// The alpha of an item's color; 255 for items without one.
fn item_alpha(item: &DisplayItem) -> u8 {
    match item {
        DisplayItem::Text(run) => run.color.a,
        DisplayItem::Rect { color, .. }
        | DisplayItem::Polygon { color, .. }
        | DisplayItem::Line { color, .. }
        | DisplayItem::Polyline { color, .. }
        | DisplayItem::Path { color, .. }
        | DisplayItem::StrokePath { color, .. } => color.a,
        DisplayItem::Image { alpha, .. } => *alpha,
        DisplayItem::Gradient { .. }
        | DisplayItem::PushClip(_)
        | DisplayItem::PushClipPath { .. }
        | DisplayItem::PushClipText(_)
        | DisplayItem::PushTransform(_)
        | DisplayItem::PopClip
        | DisplayItem::PushGroup(_)
        | DisplayItem::PopGroup
        | DisplayItem::Backdrop(_) => 255,
    }
}

/// The resource name of the graphics state for an alpha value.
fn alpha_name(alpha: u8) -> String {
    format!("A{alpha}")
}

/// An item of a `TJ` array, or what goes between `TJ` operators.
enum Positioned {
    Glyph(u16),
    Adjust(f32),
    /// The text rise of the glyphs that follow, in points.
    Rise(f32),
    /// Starts a span of glyphs that copy as this text.
    ActualText(String),
    EndActualText,
}
