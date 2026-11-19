//! Box tree, layout, text shaping, fragmentation into pages, and display lists.
//!
//! [`layout`] runs the pipeline: box tree ([`boxes`]), block and inline layout into fragments
//! ([`fragment`]), painting into display items, and pagination into [`display::Document`].

mod background;
mod border_image;
pub mod boxes;
mod carry;
mod color_glyphs;
pub mod display;
mod effects;
mod filter;
mod flex;
mod floats;
mod flow;
mod font;
mod fonts;
pub mod fragment;
mod gradient;
mod grid;
pub mod images;
mod inline;
mod links;
pub mod margins;
mod mask;
mod multicol;
pub mod paginate;
mod paint;
pub mod picture;
mod position;
mod raster;
mod rounded;
mod ruby;
mod shadow;
mod shapes;
mod svg;
mod svg_element;
mod svg_shapes;
pub mod table;
mod text;
mod transform;
mod units;
mod writing;

use rustyprint_css::page::PageOrientation;
use rustyprint_css::{ContainerSizes, StyleOptions, Styles, compute_styles_in};
use rustyprint_dom::{Document, local_name};

pub use carry::{Carried, Open, let_go};
pub use display::{
    Bookmark, Color, Destination, DisplayItem, FillRule, Link, LinkTarget, Page, Path, PathSegment,
    Point, Rect, Size, TextRun, TextStroke, Upright,
};
pub use font::{Font, FontError, VerticalMetrics};
pub use fonts::{FontChoice, font_metrics, select, select_font};
pub use fragment::{Sides, Turn};
pub use images::{Image, ImageCache, ImageData, Images};
pub use margins::{DeferredMargins, LaterTotal, MarginBoxFragment};
pub use paginate::LaidPage;
pub use svg::PixelBudget;
pub use text::{Glyph, shape};

/// The page box: its size and margins, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageOptions {
    pub size: Size,
    pub margin: Sides,
}

impl PageOptions {
    /// The page area, where content goes.
    pub fn area(&self) -> Size {
        Size::new(
            (self.size.width - self.margin.horizontal()).max(1.0),
            (self.size.height - self.margin.vertical()).max(1.0),
        )
    }
}

/// Lays out a styled document as one continuous page area, without paginating. Fragment
/// positions start at the top-left corner of the page area.
pub fn layout_tree(
    document: &Document,
    styles: &Styles,
    page: &PageOptions,
    images: &Images,
) -> Option<fragment::BoxFragment> {
    let area = page.area();
    let built = boxes::build_with_targets(document, styles, images, &boxes::Targets::default())?;
    // Page numbers of link targets come from the paginated layout.
    let root = if built.uses_targets {
        match paginated(document, styles, page, images, 0, None) {
            Ok(paginated) => paginated.built.root,
            Err(_) => built.root,
        }
    } else {
        built.root
    };
    // A vertical root turns about the page area, so what fits on the first page is where
    // it would be there.
    let logical = writing::page_area(&root, area);
    let mut fragment = flow::layout_root(&root, logical.width, logical.height, None, None).0;
    writing::root_to_page(&mut fragment, logical);
    Some(fragment)
}

/// How many times styles are computed again for container queries, at most.
const CONTAINER_PASSES: usize = 8;

/// Computes the styles of a document. Container queries and container units depend on the
/// sizes of their containers, so a document with size containers is laid out, and styled
/// again with their sizes, until the sizes stop changing.
pub fn style(
    document: &Document,
    options: &StyleOptions,
    page: &PageOptions,
    images: &Images,
) -> Styles {
    let mut sizes = ContainerSizes::new();
    let mut styles = compute_styles_in(document, options, &sizes);
    for _ in 0..CONTAINER_PASSES {
        if !styles.size_containers {
            break;
        }
        let Some(tree) = layout_tree(document, &styles, page, images) else {
            break;
        };
        let mut laid = ContainerSizes::new();
        for (node, _, fragment) in tree.element_boxes() {
            if let fragment::ElementBox::Block(block) = fragment
                && block.style.container_type.size.is_some()
            {
                let content = block.content_box();
                laid.entry(node)
                    .or_insert((content.width.max(0.0), content.height.max(0.0)));
            }
        }
        if laid == sizes {
            break;
        }
        sizes = laid;
        styles = compute_styles_in(document, options, &sizes);
    }
    styles
}

/// How many times layout runs to let `target-counter()` page numbers settle.
const TARGET_PASSES: usize = 3;

/// A box tree and its pages, with where each page continues the root from.
struct Paginated {
    built: boxes::Built,
    pages: Vec<LaidPage>,
    starts: Vec<Option<flow::BreakToken>>,
}

/// Builds the box tree and paginates it. Content that uses `target-*()` is built again with
/// the pages its targets landed on, until they stop changing. With `carried`, the pages
/// are those of a flow from its open page on.
fn paginated(
    document: &Document,
    styles: &Styles,
    page: &PageOptions,
    images: &Images,
    before: usize,
    carried: Option<&Carried>,
) -> Result<Paginated, String> {
    let mut targets = boxes::Targets::default();
    let mut pass = 0;
    loop {
        pass += 1;
        let mut built = boxes::build_with_targets(document, styles, images, &targets)
            .unwrap_or_else(|| boxes::Built {
                root: boxes::BlockBox {
                    node: None,
                    style: std::sync::Arc::new(rustyprint_css::ComputedValues::initial()),
                    contents: boxes::BlockContents::Blocks(Vec::new()),
                    page: None,
                    marker: None,
                    first_line: Vec::new(),
                    legend: None,
                    center_contents: false,
                    shape_image: None,
                    replaced: None,
                },
                running: Vec::new(),
                targets: Default::default(),
                uses_targets: false,
                uses_total: false,
            });
        let start = match carried {
            Some(carried) => carried.apply(document, &mut built.root)?,
            None => paginate::Start::default(),
        };
        let (pages, starts) = paginate::pages(&built.root, styles, page, before, &start);
        if !built.uses_targets || pass == TARGET_PASSES {
            return Ok(Paginated {
                built,
                pages,
                starts,
            });
        }
        let first = links::first_pages(&pages);
        let mut found = boxes::Targets {
            elements: built.targets.clone(),
            pages: pages.len(),
        };
        for (node, target) in &mut found.elements {
            target.page = first.get(node).map(|page| before + page);
        }
        if found == targets {
            return Ok(Paginated {
                built,
                pages,
                starts,
            });
        }
        targets = found;
    }
}

/// Lays out a styled document page by page, on pages that `@page` rules style. `page` is
/// the size and margins of pages that the rules leave alone.
pub fn layout_pages(
    document: &Document,
    styles: &Styles,
    page: &PageOptions,
    images: &Images,
) -> Vec<LaidPage> {
    layout_pages_after(document, styles, page, images, 0, None)
}

/// Lays out a styled document as [`layout_pages`] does, as a document that comes after
/// `before` pages already laid out, such as one record of a set that is printed as one
/// file: its page numbers and its left and right pages carry on from them. Its first page
/// is still a `:first` page.
///
/// `counter(pages)` is the number of the document's own pages. With `later`, it is the
/// number of pages of the whole set instead, which is known when the set ends: a page
/// whose margin boxes show it gets them as [`LaidPage::deferred`], for
/// [`paint_deferred_margins`], and they are not among its margin boxes.
/// `target-counter(url, pages)` in the content still counts the document's own pages, with
/// a warning.
pub fn layout_pages_after(
    document: &Document,
    styles: &Styles,
    page: &PageOptions,
    images: &Images,
    before: usize,
    later: Option<&LaterTotal>,
) -> Vec<LaidPage> {
    match layout_flow(document, styles, page, images, before, later, None) {
        Ok((pages, _)) => pages,
        // Only a flow that carries on from an open page can fail.
        Err(_) => Vec::new(),
    }
}

/// Lays out a styled document as [`layout_pages_after`] does, as a flow that is laid out a
/// piece at a time. Its last page is open: the next piece may put content on it. So the
/// caller writes every page but the last, lets go of what those pages have done with
/// ([`let_go`]), adds the next piece to the document and lays it out again with what it
/// kept, as `carried`. The pages are then those from the open page on, which is laid out
/// again with the content that was added, and `before` is how many pages come before that
/// page. A table that is a child of `<body>` keeps the columns it was first laid out with.
///
/// Fails when the document cannot carry on where it was: when rows added to a table have
/// more columns than the table was laid out with.
pub fn layout_flow(
    document: &Document,
    styles: &Styles,
    page: &PageOptions,
    images: &Images,
    before: usize,
    later: Option<&LaterTotal>,
    carried: Option<&Carried>,
) -> Result<(Vec<LaidPage>, Open), String> {
    inline::set_quirks(document.quirks_mode() != rustyprint_dom::QuirksMode::NoQuirks);
    let Paginated {
        built,
        mut pages,
        mut starts,
    } = paginated(document, styles, page, images, before, carried)?;
    let flow_pages = carried.map_or(0, Carried::flow_pages) + pages.len().saturating_sub(1);
    let open = carry::open(
        document,
        styles,
        &built.root,
        carried,
        starts.pop().flatten(),
        flow_pages,
    );
    if later.is_some() && built.uses_total {
        images.warn(
            "target-counter(…, pages) counts the pages of its own record, not of the set".into(),
        );
    }
    let running = built.running;
    let order: std::collections::HashMap<_, _> = document
        .descendants(document.root())
        .enumerate()
        .map(|(index, node)| (node, index))
        .collect();
    // What comes before the element that an open page starts in is on earlier pages.
    let earlier = carried
        .and_then(Carried::starts_in)
        .and_then(|node| order.get(&node))
        .map(|position: &usize| position.saturating_sub(1));
    margins::all(
        &mut pages,
        &running,
        &order,
        &styles.counter_styles,
        images,
        (before, earlier),
        later,
    );
    Ok((pages, open))
}

/// What the margin boxes that waited for the number of pages paint, on their page of a
/// set of `total` pages.
pub fn paint_deferred_margins(
    deferred: &DeferredMargins,
    total: usize,
    images: &Images,
) -> Vec<DisplayItem> {
    let size = deferred.size();
    let sheet = Rect::new(0.0, 0.0, size.width, size.height);
    let mut items = Vec::new();
    for margin_box in deferred.lay_out(total) {
        items.extend(paint::paint(&margin_box.fragment, images, sheet).items);
    }
    effects::resolve_backdrops(items)
}

/// The link areas of each laid-out page, and the document's bookmarks from its headings.
pub fn links_and_bookmarks(
    document: &Document,
    pages: &[LaidPage],
) -> (Vec<Vec<Link>>, Vec<Bookmark>) {
    links::collect(document, pages)
}

/// The display items a box and its descendants paint, relative to the box's parent. Fixed
/// backgrounds are positioned in the box's border box.
pub fn paint_fragment(fragment: &fragment::BoxFragment, images: &Images) -> Vec<DisplayItem> {
    effects::resolve_backdrops(paint::paint(fragment, images, fragment.rect).items)
}

/// Draws display items to pixels: what they paint inside `rect`, with the engine's own
/// drawing of filters and masks, at two pixels for each CSS pixel. `None` if `rect` is empty.
pub fn rasterize(items: &[DisplayItem], rect: Rect) -> Option<Image> {
    let mut raster = raster::Raster::new(rect)?;
    raster::draw(items, &mut raster);
    Some(raster.to_image())
}

/// The document's title for PDF metadata: the text of its first `<title>`, with whitespace
/// collapsed, unless that is empty.
pub fn document_title(document: &Document) -> Option<String> {
    document
        .find_html(&local_name!("title"))
        .map(|title| {
            document
                .text_content(title)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|title| !title.is_empty())
}

/// Lays out a styled document into pages, loading the images it uses from `images`.
pub fn layout(
    document: &Document,
    styles: &Styles,
    page: &PageOptions,
    images: &Images,
) -> display::Document {
    let laid_pages = layout_pages(document, styles, page, images);
    paint_pages(document, styles, &laid_pages, images, false)
}

/// How many times wider than its page area a page's content is laid out, for the page
/// where that is the most: 1 when every page's content fits. This is what a browser's print
/// shrinks a document by.
pub fn overflow(pages: &[LaidPage]) -> f32 {
    let factor = |page: &LaidPage| {
        let root = page.root.as_ref()?;
        (page.area.width > 0.0).then(|| root.overflow_right() / page.area.width)
    };
    pages.iter().filter_map(factor).fold(1.0, f32::max)
}

/// Leaves out the items, and the glyphs at the end of a text run, that start at or past
/// `edge`: a clip there hides them, and text that is not on the page should not be in the
/// document either. What is inside a transform is kept.
fn cut_right(items: impl Iterator<Item = DisplayItem>, edge: f32) -> Vec<DisplayItem> {
    // Whether each open clip or transform is a transform.
    let mut open: Vec<bool> = Vec::new();
    let mut out = Vec::new();
    for mut item in items {
        match &mut item {
            DisplayItem::PushTransform(_) => open.push(true),
            DisplayItem::PushClip(_)
            | DisplayItem::PushClipPath { .. }
            | DisplayItem::PushClipText(_) => open.push(false),
            DisplayItem::PopClip => {
                open.pop();
            }
            _ if open.contains(&true) => {}
            DisplayItem::Text(run) => {
                let mut x = run.origin.x;
                let visible = run.glyphs.iter().take_while(|glyph| {
                    let starts = x;
                    x += glyph.x_advance;
                    starts < edge
                });
                let visible = visible.count();
                if visible == 0 {
                    continue;
                }
                run.glyphs.truncate(visible);
            }
            other => {
                if other.bounds().is_some_and(|bounds| bounds.x >= edge) {
                    continue;
                }
            }
        }
        out.push(item);
    }
    out
}

/// Paints laid-out pages. `clip` cuts off what the content paints to the right of its page
/// area, as a browser's print does with content that is still too wide when shrunk as far
/// as it goes.
pub fn paint_pages(
    document: &Document,
    styles: &Styles,
    laid_pages: &[LaidPage],
    images: &Images,
    clip: bool,
) -> display::Document {
    let title = document_title(document);
    let (mut links, bookmarks) = links_and_bookmarks(document, laid_pages);
    picture::register_elements(document, laid_pages, images);
    let pages = laid_pages
        .iter()
        .zip(links.iter_mut())
        .map(|(laid, links)| {
            let style = &laid.style;
            let bleed = style.used_bleed();
            let mut items = paint::paint_page(laid, images, bleed);
            if let Some(root) = &laid.root {
                let area = laid.area;
                let viewport = Rect::new(0.0, 0.0, area.width, area.height);
                let painted = paint::paint(root, images, viewport);
                let content = painted
                    .items
                    .iter()
                    .map(|item| item.translated(area.x, area.y));
                if clip {
                    let Size { width, height } = laid.size;
                    items.push(DisplayItem::PushClip(Rect::new(
                        -width,
                        -height,
                        width + area.right(),
                        3.0 * height,
                    )));
                    items.extend(cut_right(content, area.right()));
                    items.push(DisplayItem::PopClip);
                } else {
                    items.extend(content);
                }
            }
            let sheet = Rect::new(0.0, 0.0, laid.size.width, laid.size.height);
            for margin_box in &laid.margin_boxes {
                items.extend(paint::paint(&margin_box.fragment, images, sheet).items);
            }
            Page {
                size: laid.size,
                items: effects::resolve_backdrops(items),
                bleed,
                marks: display::PrintMarks {
                    crop: style.marks.crop,
                    cross: style.marks.cross,
                },
                rotate: match style.orientation {
                    PageOrientation::Upright => 0,
                    PageOrientation::RotateLeft => 270,
                    PageOrientation::RotateRight => 90,
                },
                links: std::mem::take(links),
            }
        })
        .collect();
    let color_profiles = styles
        .color_profiles
        .iter()
        .map(|profile| display::ColorProfile {
            data: profile.data.clone(),
            components: profile.components,
        })
        .collect();
    let device_cmyk = styles
        .color_profiles
        .iter()
        .position(|profile| &*profile.name == "device-cmyk" && profile.components == 4)
        .map(|index| index as u32);
    display::Document {
        title,
        pages,
        bookmarks,
        color_profiles,
        device_cmyk,
    }
}
