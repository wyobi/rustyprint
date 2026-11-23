//! Page-margin boxes (CSS Paged Media 3 § 5): running headers, footers and page numbers in
//! the margins of a page.

use rustyprint_css::ComputedValues;
use rustyprint_css::counter_style::CounterStyles;
use rustyprint_css::page::MarginBox;
use rustyprint_css::properties::types::{
    AlignmentBaseline, BaselineShift, Content, ContentItem, ElementPolicy, ImageValue,
    Size as CssSize,
};

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use rustyprint_css::page::PageStyle;
use rustyprint_dom::NodeId;

use crate::boxes::{BlockBox, BlockContents, InlineItem, RunningElement, image_item};
use crate::display::{Rect, Size};
use crate::flow::{self, BlockKind, ContainingBlock};
use crate::fragment::{BoxFragment, Fragment};
use crate::images::Images;
use crate::paginate::LaidPage;
use crate::picture::Picture;

/// A laid-out margin box, positioned in page coordinates.
#[derive(Debug, Clone)]
pub struct MarginBoxFragment {
    pub name: MarginBox,
    pub fragment: BoxFragment,
}

/// A piece of a margin box's content.
#[derive(Debug, PartialEq)]
enum Piece {
    Text(String),
    Image(ImageValue),
}

/// The text and images of a margin box's `content`, or `None` if the box is not generated.
/// `number` is the page's number and `total` the number of pages, for `counter(page)` and
/// `counter(pages)`; other counters are zero.
fn content_pieces(
    style: &ComputedValues,
    number: usize,
    total: usize,
    counter_styles: &CounterStyles,
) -> Option<Vec<Piece>> {
    let Content::Items { items, .. } = &style.content else {
        return None;
    };
    let mut pieces = Vec::new();
    let counter = |name: &str| match name {
        "page" => number as i32,
        "pages" => total as i32,
        _ => 0,
    };
    let mut text = String::new();
    for item in items.iter() {
        match item {
            ContentItem::String(s) => text.push_str(s),
            ContentItem::Counter { name, style } | ContentItem::Counters { name, style, .. } => {
                let formatted = counter_styles.format(counter(name), style);
                text.push_str(&counter_styles.without_images(&formatted));
            }
            ContentItem::Image(image) => {
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Image(image.clone()));
            }
            // Quotes have no nesting in a margin box, and running elements replace the text.
            // Targets and leaders only apply in the document.
            ContentItem::Element { .. }
            | ContentItem::TargetCounter { .. }
            | ContentItem::TargetCounters { .. }
            | ContentItem::TargetText { .. }
            | ContentItem::Leader(_)
            | ContentItem::OpenQuote
            | ContentItem::CloseQuote
            | ContentItem::NoOpenQuote
            | ContentItem::NoCloseQuote => {}
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Some(pieces)
}

/// A generated margin box before it is placed.
struct Generated {
    name: MarginBox,
    block: BlockBox,
}

/// Finds the running element a page shows for `element(name, policy)`.
type FindRunning<'a> = &'a dyn Fn(&str, ElementPolicy) -> Option<&'a BlockBox>;

/// The three boxes of each edge of a page, which share the edge between them: the top, the
/// bottom, the left and the right.
const EDGES: [[MarginBox; 3]; 4] = {
    use MarginBox::*;
    [
        [TopLeft, TopCenter, TopRight],
        [BottomLeft, BottomCenter, BottomRight],
        [LeftTop, LeftMiddle, LeftBottom],
        [RightTop, RightMiddle, RightBottom],
    ]
};

/// The four boxes in the corners of a page.
const CORNERS: [MarginBox; 4] = {
    use MarginBox::*;
    [
        TopLeftCorner,
        TopRightCorner,
        BottomRightCorner,
        BottomLeftCorner,
    ]
};

/// The sheet a page's margin boxes are placed on.
#[derive(Debug, Clone, Copy)]
struct Sheet {
    /// The page's size.
    size: Size,
    /// The page without its margins.
    page_box: Rect,
}

impl Sheet {
    fn of(page: &LaidPage) -> Self {
        Self {
            size: page.size,
            page_box: page.page_box,
        }
    }

    /// The size a box wants along edge `edge` of [`EDGES`], which the edge is divided by:
    /// its width on the top and bottom edges, and on the left and right edges the height
    /// its content needs at the margin's width.
    fn wanted(&self, edge: usize, block: &BlockBox) -> f32 {
        let pb = self.page_box;
        match edge {
            0 | 1 => outer_width(block, pb.width),
            2 => content_height(block, pb.x),
            _ => content_height(block, self.size.width - pb.right()),
        }
    }
}

/// The running element that the `content` of a margin box names with `element()`.
fn element_of(style: &ComputedValues) -> Option<(&str, ElementPolicy)> {
    let Content::Items { items, .. } = &style.content else {
        return None;
    };
    items.iter().find_map(|item| match item {
        ContentItem::Element { name, policy } => Some((&**name, *policy)),
        _ => None,
    })
}

/// Whether the `content` of a margin box shows the number of pages.
fn shows_total(style: &ComputedValues) -> bool {
    let Content::Items { items, .. } = &style.content else {
        return false;
    };
    items.iter().any(|item| match item {
        ContentItem::Counter { name, .. } | ContentItem::Counters { name, .. } => {
            &**name == "pages"
        }
        _ => false,
    })
}

/// A margin box with `style` and the `pieces` of its content. `element()` puts a running
/// element in the box instead of text: `element` is the one the page shows, if the content
/// names one. `picture` gives the image of each image piece, in their order.
fn generated(
    name: MarginBox,
    style: &Arc<ComputedValues>,
    pieces: Vec<Piece>,
    element: Option<Option<&BlockBox>>,
    picture: &mut dyn FnMut(&ImageValue) -> Option<Picture>,
) -> Generated {
    let contents = match element {
        Some(Some(block)) => BlockContents::Blocks(vec![block.clone()]),
        Some(None) => BlockContents::Blocks(Vec::new()),
        None => BlockContents::Inline(
            pieces
                .into_iter()
                .filter_map(|piece| match piece {
                    Piece::Text(text) => Some(InlineItem::Text {
                        text,
                        style: style.clone(),
                        lang: None,
                    }),
                    Piece::Image(image) => {
                        let picture = picture(&image)?;
                        let size = picture.natural_size(style);
                        Some(image_item(picture, size, style))
                    }
                })
                .collect(),
        ),
    };
    Generated {
        name,
        block: BlockBox {
            node: None,
            style: style.clone(),
            contents,
            page: None,
            marker: None,
            first_line: Vec::new(),
            legend: None,
            center_contents: false,
            shape_image: None,
            replaced: None,
        },
    }
}

/// Generates the margin boxes of a page from its style.
fn generate(
    style: &PageStyle,
    number: usize,
    total: usize,
    running: FindRunning<'_>,
    counter_styles: &CounterStyles,
    images: &Images,
) -> Vec<Generated> {
    style
        .margin_boxes
        .iter()
        .filter_map(|(name, style)| {
            let pieces = content_pieces(style, number, total, counter_styles)?;
            let element = element_of(style).map(|(name, policy)| running(name, policy));
            let mut picture = |image: &ImageValue| Picture::resolve(image, images, style);
            Some(generated(*name, style, pieces, element, &mut picture))
        })
        .collect()
}

/// Places generated margin boxes on a page. `middles` is the size of the middle box of
/// each edge of [`EDGES`] that is not among `generated` and still takes its room.
fn place(
    generated: &[Generated],
    sheet: Sheet,
    middles: [Option<f32>; 4],
) -> Vec<MarginBoxFragment> {
    if generated.is_empty() {
        return Vec::new();
    }
    let find = |name: MarginBox| generated.iter().find(|g| g.name == name);

    let (width, height) = (sheet.size.width, sheet.size.height);
    let pb = sheet.page_box;
    let (top, left) = (pb.y, pb.x);
    let right = width - pb.right();
    let bottom = height - pb.bottom();
    let mut out = Vec::new();
    let mut place = |g: &Generated, rect: Rect| {
        out.push(MarginBoxFragment {
            name: g.name,
            fragment: lay_out(&g.block, rect),
        });
    };

    // The corners.
    let corners = [
        Rect::new(0.0, 0.0, left, top),
        Rect::new(width - right, 0.0, right, top),
        Rect::new(width - right, height - bottom, right, bottom),
        Rect::new(0.0, height - bottom, left, bottom),
    ];
    for (name, rect) in CORNERS.into_iter().zip(corners) {
        if let Some(g) = find(name) {
            place(g, rect);
        }
    }

    // What each box of an edge gets of it.
    let shares = |edge: usize, span: f32| {
        let boxes = EDGES[edge].map(find);
        let mut sizes = boxes.map(|g| g.map(|g| sheet.wanted(edge, &g.block)));
        if boxes[1].is_none() {
            sizes[1] = middles[edge];
        }
        (boxes, distribute(span, sizes))
    };

    // The top and bottom edges divide their width between three boxes.
    for (edge, y, h) in [(0, 0.0, top), (1, height - bottom, bottom)] {
        let (boxes, widths) = shares(edge, pb.width);
        let mut x = left;
        for (g, w) in boxes.iter().zip(widths) {
            if let Some(g) = g {
                place(g, Rect::new(x, y, w, h));
            }
            x += w;
        }
    }

    // The left and right edges divide their height, by the height each box's content
    // needs at the margin's width.
    for (edge, x, w) in [(2, 0.0, left), (3, width - right, right)] {
        let (boxes, heights) = shares(edge, pb.height);
        let mut y = top;
        for (g, h) in boxes.iter().zip(heights) {
            if let Some(g) = g {
                place(g, Rect::new(x, y, w, h));
            }
            y += h;
        }
    }
    out
}

/// What a margin box that is laid out later keeps of the document, besides its style: the
/// content that only the document can give.
#[derive(Debug, Clone, Default)]
struct Held {
    /// The running element of `element()`; `Some(None)` when the page shows none.
    element: Option<Option<Arc<BlockBox>>>,
    /// The images of the content, in order.
    pictures: Vec<Option<Picture>>,
}

/// The margin boxes of one page that show the number of pages of a set that is still
/// being printed, to lay out with [`crate::paint_deferred_margins`] once the set is
/// whole.
///
/// It keeps the page's style, which the pages of a document share, its number, its size
/// and where its margins are, and the names of the boxes: nothing of what the page
/// paints. A box that shares an edge with one that shows the total is kept as well when
/// its place depends on how wide the total is; if such a box holds a running element or an
/// image, that is kept for it.
#[derive(Debug, Clone)]
pub struct DeferredMargins {
    style: Arc<PageStyle>,
    counter_styles: Arc<CounterStyles>,
    number: usize,
    sheet: Sheet,
    /// The boxes to lay out.
    boxes: Vec<(MarginBox, Held)>,
    /// The size of the middle box of each edge that was painted with the page.
    middles: [Option<f32>; 4],
}

impl DeferredMargins {
    /// Lays the boxes out for a set of `total` pages.
    pub(crate) fn lay_out(&self, total: usize) -> Vec<MarginBoxFragment> {
        let generated: Vec<Generated> = self
            .boxes
            .iter()
            .filter_map(|(name, held)| {
                let style = find_style(&self.style, *name)?;
                let pieces = content_pieces(style, self.number, total, &self.counter_styles)?;
                let element = held.element.as_ref().map(|element| element.as_deref());
                let mut pictures = held.pictures.iter();
                let mut picture = |_: &ImageValue| pictures.next().cloned().flatten();
                Some(generated(*name, style, pieces, element, &mut picture))
            })
            .collect();
        place(&generated, self.sheet, self.middles)
    }

    /// The size of the page.
    pub(crate) fn size(&self) -> Size {
        self.sheet.size
    }
}

/// That the pages are those of a set whose number of pages is known later: margin boxes
/// that show it are left for when it is. One of these serves every document of the set.
#[derive(Debug, Default)]
pub struct LaterTotal {
    /// The counter styles of the document laid out last, which the next one shares if its
    /// own are the same.
    counter_styles: RefCell<Option<Arc<CounterStyles>>>,
}

impl LaterTotal {
    pub fn new() -> Self {
        Self::default()
    }

    fn shared(&self, counter_styles: &CounterStyles) -> Arc<CounterStyles> {
        let mut last = self.counter_styles.borrow_mut();
        match &*last {
            Some(shared) if **shared == *counter_styles => shared.clone(),
            _ => last.insert(Arc::new(counter_styles.clone())).clone(),
        }
    }
}

/// Which margin boxes of a page wait for the number of pages: those that show it, and
/// those whose place on their edge depends on the size of one that does. With them comes
/// the size of each middle box that does not wait and gives the boxes beside it their
/// room.
fn waiting(generated: &[Generated], sheet: Sheet) -> (Vec<MarginBox>, [Option<f32>; 4]) {
    let find = |name: MarginBox| generated.iter().find(|g| g.name == name);
    let shows = |name: MarginBox| find(name).is_some_and(|g| shows_total(&g.block.style));
    let mut names: Vec<MarginBox> = CORNERS.into_iter().filter(|name| shows(*name)).collect();
    let mut middles = [None; 4];
    for (edge, boxes) in EDGES.into_iter().enumerate() {
        if !boxes.into_iter().any(shows) {
            continue;
        }
        match find(boxes[1]) {
            // A middle box takes the room it needs and the boxes beside it share the
            // rest equally, so only a box that shows the total depends on it.
            Some(middle) if !shows(boxes[1]) => {
                middles[edge] = Some(sheet.wanted(edge, &middle.block));
                names.extend(boxes.into_iter().filter(|name| shows(*name)));
            }
            _ => names.extend(boxes.into_iter().filter(|name| find(*name).is_some())),
        }
    }
    (names, middles)
}

/// Divides `span` between the start, middle and end boxes of an edge (§ 5.3.2), given the
/// size each generated box's content wants. A middle box is centered with the room it
/// needs, and the boxes beside it share the rest equally; without one, the start and end
/// boxes share the span in proportion to what they want.
fn distribute(span: f32, sizes: [Option<f32>; 3]) -> [f32; 3] {
    match sizes {
        [_, Some(middle), _] => {
            let middle = middle.min(span);
            let side = (span - middle) / 2.0;
            [side, middle, side]
        }
        [Some(a), None, Some(c)] => {
            let total = a + c;
            let a = if total > 0.0 {
                span * a / total
            } else {
                span / 2.0
            };
            [a, 0.0, span - a]
        }
        [Some(_), None, None] => [span, 0.0, 0.0],
        [None, None, Some(_)] => [0.0, 0.0, span],
        [None, None, None] => [0.0; 3],
    }
}

/// The width a margin box wants along a top or bottom edge of `span`: a percentage width
/// refers to the edge, other widths are the content's max-content width.
fn outer_width(block: &BlockBox, span: f32) -> f32 {
    match &block.style.width {
        CssSize::Value(value) if value.as_length().is_none() => {
            value.resolve(span) + flow::outer_extra(&block.style)
        }
        _ => flow::intrinsic_outer(block).1,
    }
}

/// The margin-box height a box's content needs at `width`.
fn content_height(block: &BlockBox, width: f32) -> f32 {
    let cb = ContainingBlock {
        width,
        height: None,
        rtl: false,
    };
    let laid = flow::layout_block(block, cb, BlockKind::InFlow, &[], None, None);
    laid.fragment.rect.height + laid.margin.vertical()
}

/// Lays out a margin box in `rect`: its margin box fills the rectangle, and its content is
/// aligned vertically as `vertical-align` asks.
fn lay_out(block: &BlockBox, rect: Rect) -> BoxFragment {
    let style = &block.style;
    let cb = ContainingBlock {
        width: rect.width,
        height: Some(rect.height),
        rtl: false,
    };
    let laid = flow::layout_block(block, cb, BlockKind::InFlow, &[], None, None);
    let mut fragment = laid.fragment;
    let margin = laid.margin;
    fragment.rect.x = rect.x + margin.left;
    fragment.rect.y = rect.y + margin.top;
    if matches!(style.height, CssSize::Auto) {
        let content = fragment.rect.height;
        let available = (rect.height - margin.vertical()).max(content);
        let fraction = match (&style.baseline_shift, style.alignment_baseline) {
            (BaselineShift::Top, _) => 0.0,
            (BaselineShift::Bottom, _) => 1.0,
            (_, AlignmentBaseline::Middle | AlignmentBaseline::Central) => 0.5,
            (BaselineShift::Center, _) => 0.5,
            _ => 0.0,
        };
        let shift = (available - content) * fraction;
        for child in &mut fragment.children {
            match child {
                Fragment::Line(line) => line.rect.y += shift,
                Fragment::Box(child) => child.rect.y += shift,
            }
        }
        fragment.rect.height = available;
    }
    fragment
}

/// Lays out the margin boxes of every page, numbered from one after the `preceding` pages
/// that come before the first. `running` are the running elements in document order, and `order`
/// gives each element's position in the document. With `earlier`, the pages are those of a
/// flow from a page that is laid out again, and the elements up to that position are on
/// the pages before it.
///
/// `counter(pages)` is the number of `pages`. With `later`, the pages are part of a set
/// whose number of pages is not known yet: the boxes that show it are not laid out, and
/// the page gets what lays them out later instead.
pub(crate) fn all(
    pages: &mut [LaidPage],
    running: &[RunningElement],
    order: &HashMap<NodeId, usize>,
    counter_styles: &CounterStyles,
    images: &Images,
    (preceding, earlier): (usize, Option<usize>),
    later: Option<&LaterTotal>,
) {
    let total = pages.len();
    let position = |node: Option<NodeId>| node.and_then(|n| order.get(&n).copied());
    // The running elements kept for later, each once for all the pages that show it.
    let mut kept: HashMap<usize, Arc<BlockBox>> = HashMap::new();
    // A running element is assigned on the page where the content after it is laid out:
    // after the last element of the page before, up to the last element of this page.
    let mut previous_end = earlier;
    for (index, page) in pages.iter_mut().enumerate() {
        let end = page
            .root
            .as_ref()
            .and_then(|root| {
                root.element_boxes()
                    .iter()
                    .filter_map(|(node, ..)| order.get(node).copied())
                    .max()
            })
            .or(previous_end);
        let on_page = |p: usize| previous_end.is_none_or(|s| p > s) && end.is_some_and(|e| p <= e);
        let before = |p: usize| previous_end.is_some_and(|s| p <= s);
        let find = |name: &str, policy: ElementPolicy| -> Option<&BlockBox> {
            let named = || running.iter().filter(|r| &*r.name == name);
            let here: Vec<&RunningElement> = named()
                .filter(|r| position(r.block.node).is_some_and(on_page))
                .collect();
            let earlier = named().rfind(|r| position(r.block.node).is_some_and(before));
            let chosen = match policy {
                ElementPolicy::First | ElementPolicy::Start => here.first().copied().or(earlier),
                ElementPolicy::Last => here.last().copied().or(earlier),
                ElementPolicy::FirstExcept if here.is_empty() => earlier,
                ElementPolicy::FirstExcept => None,
            };
            chosen.map(|r| &r.block)
        };
        let number = preceding + index + 1;
        let sheet = Sheet::of(page);
        // Until the total is known, the boxes that wait for it stand in with this one.
        let generated = generate(&page.style, number, total, &find, counter_styles, images);
        let mut boxes = place(&generated, sheet, [None; 4]);
        let (names, middles) = match later {
            Some(_) => waiting(&generated, sheet),
            None => (Vec::new(), [None; 4]),
        };
        if let (Some(later), false) = (later, names.is_empty()) {
            boxes.retain(|margin_box| !names.contains(&margin_box.name));
            let held = |name: MarginBox| {
                let style = find_style(&page.style, name)?;
                let element = element_of(style).map(|(name, policy)| {
                    find(name, policy).map(|block| {
                        kept.entry(std::ptr::from_ref(block).addr())
                            .or_insert_with(|| Arc::new(block.clone()))
                            .clone()
                    })
                });
                let pictures = match (&element, &style.content) {
                    (None, Content::Items { items, .. }) => items
                        .iter()
                        .filter_map(|item| match item {
                            ContentItem::Image(image) => {
                                Some(Picture::resolve(image, images, style))
                            }
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                Some((name, Held { element, pictures }))
            };
            page.deferred = Some(DeferredMargins {
                style: page.style.clone(),
                counter_styles: later.shared(counter_styles),
                number,
                sheet,
                boxes: names.into_iter().filter_map(held).collect(),
                middles,
            });
        }
        page.margin_boxes = boxes;
        previous_end = end;
    }
}

/// The style of a margin box of a page.
fn find_style(page: &PageStyle, name: MarginBox) -> Option<&Arc<ComputedValues>> {
    page.margin_boxes
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, style)| style)
}

#[cfg(test)]
mod tests {
    use rustyprint_css::counter_style::CounterStyleRef;

    use super::*;

    #[test]
    fn middle_boxes_are_centered() {
        assert_eq!(
            distribute(100.0, [Some(10.0), Some(20.0), None]),
            [40.0, 20.0, 40.0]
        );
        assert_eq!(
            distribute(100.0, [Some(30.0), None, Some(10.0)]),
            [75.0, 0.0, 25.0]
        );
        assert_eq!(
            distribute(100.0, [None, None, Some(5.0)]),
            [0.0, 0.0, 100.0]
        );
    }

    #[test]
    fn page_counters() {
        let mut style = ComputedValues::initial();
        let items: Arc<[ContentItem]> = Arc::from([
            ContentItem::String(Arc::from("Page ")),
            ContentItem::Counter {
                name: Arc::from("page"),
                style: CounterStyleRef::Name(Arc::from("upper-roman")),
            },
            ContentItem::String(Arc::from(" of ")),
            ContentItem::Counter {
                name: Arc::from("pages"),
                style: CounterStyleRef::decimal(),
            },
        ]);
        style.content = Content::Items { items, alt: None };
        let counter_styles = CounterStyles::predefined();
        assert_eq!(
            content_pieces(&style, 4, 9, &counter_styles),
            Some(vec![Piece::Text("Page IV of 9".into())])
        );
        assert_eq!(
            content_pieces(&ComputedValues::initial(), 1, 1, &counter_styles),
            None
        );
    }
}
