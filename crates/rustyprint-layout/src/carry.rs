//! Flows laid out a piece at a time: what is kept of a document between two pieces, and
//! what is let go of.
//!
//! The last page of a flow is open: the next piece may put content on it. The page is laid
//! out again, with that content, from the place in the box tree where it starts, so what is
//! kept is that place and the part of the document from there on. The place is a
//! [`BreakToken`], which counts boxes from the start of their parent; the nodes that those
//! counts stood for are kept with it, so that the counts can be found again once the
//! content before them is gone.
//!
//! The elements that the next piece can add to are those that are *open*: `<body>`, and
//! the last child of an open element when nothing but white space and comments follows
//! it. The open elements are the ones still to be closed where the flow so far ends, as
//! they are in the HTML of a document cut off at that point.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rustyprint_css::Styles;
use rustyprint_css::properties::Property;
use rustyprint_css::properties::types::{Display, Float, Position, Size};
use rustyprint_dom::{Document, NodeData, NodeId, local_name};

use crate::boxes::{self, BlockBox, BlockContents};
use crate::flow::{BreakToken, Resume};
use crate::paginate::Start;
use crate::table::Frozen;

/// What a flow keeps between two of its pieces, besides its document.
#[derive(Debug, Clone, Default)]
pub struct Carried {
    start: Start,
    /// The nodes whose boxes the token of `start` counts to.
    anchor: Option<Anchor>,
    /// The columns of the tables in block boxes, as they were first laid out: the rows a
    /// table has let go of, and those it has yet to get, do not change them.
    frozen: Vec<(NodeId, Arc<Frozen>)>,
    /// The open table that the next piece can add rows to, or why the open table cannot
    /// have rows added.
    table: Option<Result<NodeId, String>>,
}

/// The nodes of the boxes that a token counts to: `<body>` in the root, then the element
/// the open page starts in at each level below it, down to the deepest whose box is a
/// block among the blocks of its parent, and the row of that one, if it is a table that
/// the page starts in the rows of.
#[derive(Debug, Clone, PartialEq)]
struct Anchor {
    path: Vec<NodeId>,
    row: Option<NodeId>,
}

/// The open page of a flow that was just laid out: what to keep of it, and the nodes of
/// its document that the pages before the open one have done with.
#[derive(Debug)]
pub struct Open {
    carried: Carried,
    removals: Vec<Removal>,
    /// How many of the pages laid out the flow has so far, up to its open page.
    pages: usize,
}

/// Nodes to remove from the start of the children of `parent`.
#[derive(Debug)]
struct Removal {
    parent: NodeId,
    nodes: Vec<NodeId>,
    elements: u32,
    counters: Vec<(String, i32)>,
}

impl Open {
    /// How many of the pages just laid out the flow has so far: those up to its open page,
    /// the page that the last element of its document starts on. The pages after it have
    /// only the ends of the elements that are still open, such as their bottom padding and
    /// borders, which the next piece pushes on: in the whole document, more content comes
    /// before them. So they are laid out again with the next piece, and are the flow's
    /// last pages only when it ends.
    pub fn pages(&self) -> usize {
        self.pages
    }
}

impl Carried {
    /// How many pages of the flow come before its open page.
    pub fn flow_pages(&self) -> usize {
        self.start.flow_pages
    }

    /// The table that the next piece can add rows to: the first `<table>` among the open
    /// elements, if its box and the boxes of the elements around it, up to `<body>`, are
    /// blocks among blocks. Fails when the open table is in another kind of box, whose
    /// size or place depends on what is in it, with what that box is.
    pub fn open_table(&self) -> Result<Option<NodeId>, String> {
        self.table.clone().transpose()
    }

    /// Where in the document the child of `<body>` is that the open page starts in, for
    /// the margin boxes: what comes before it is on earlier pages.
    pub(crate) fn starts_in(&self) -> Option<NodeId> {
        self.anchor
            .as_ref()
            .and_then(|anchor| anchor.path.get(1).copied())
    }

    /// Gives the tables of `root` the columns they keep, and finds where the open page
    /// starts in `root`, the box tree of the flow's document as it is now.
    pub(crate) fn apply(&self, document: &Document, root: &mut BlockBox) -> Result<Start, String> {
        if !self.frozen.is_empty() {
            let body = body_element(document).and_then(|body| body_box_mut(root, body));
            if let Some((_, body)) = body {
                self.freeze(body)?;
            }
        }
        let mut start = self.start.clone();
        if let (Some(token), Some(anchor)) = (&mut start.token, &self.anchor) {
            rebase(token, root, &anchor.path, anchor.row)
                .ok_or("the page a piece continues on is not in the document any more")?;
        }
        Ok(start)
    }

    /// Gives the tables in the blocks of `parent` the columns they keep.
    fn freeze(&self, parent: &mut BlockBox) -> Result<(), String> {
        let BlockContents::Blocks(children) = &mut parent.contents else {
            return Ok(());
        };
        for child in children {
            let BlockContents::Table(table) = &mut child.contents else {
                self.freeze(child)?;
                continue;
            };
            let frozen = self
                .frozen
                .iter()
                .find(|(node, _)| Some(*node) == child.node);
            if let Some((_, frozen)) = frozen {
                if frozen.widths.len() != table.columns.len() {
                    return Err(format!(
                        "a table that continues has {} columns, and the rows added to it \
                         make that {}",
                        frozen.widths.len(),
                        table.columns.len()
                    ));
                }
                table.frozen = Some(frozen.clone());
            }
        }
        Ok(())
    }
}

const GONE: Option<()> = None;

/// Sets the counts of `token`, which continues `parent`, to where the boxes of `path` are
/// among its blocks and theirs, and those of `row` in the table that ends the path.
fn rebase(
    token: &mut BreakToken,
    parent: &BlockBox,
    path: &[NodeId],
    row: Option<NodeId>,
) -> Option<()> {
    let Some((&node, rest)) = path.split_first() else {
        let Some(row) = row else {
            return Some(());
        };
        let BlockContents::Table(table) = &parent.contents else {
            return GONE;
        };
        let at = table.body_rows().position(|r| r.node == Some(row))?;
        match &mut token.resume {
            Resume::Row { index } | Resume::RowPart { index, .. } => *index = at,
            _ => return GONE,
        }
        return Some(());
    };
    let BlockContents::Blocks(children) = &parent.contents else {
        return GONE;
    };
    let at = children.iter().position(|child| child.node == Some(node))?;
    let Resume::Child { index, token } = &mut token.resume else {
        return GONE;
    };
    *index = at;
    match token {
        Some(token) => rebase(token, &children[at], rest, row),
        None if rest.is_empty() && row.is_none() => Some(()),
        None => GONE,
    }
}

/// The open child of `parent`, an open element: its last child, if that is an element and
/// nothing but white space and comments follows it.
pub fn open_element(document: &Document, parent: NodeId) -> Option<NodeId> {
    let shown = |child: &NodeId| match document.node(*child).data() {
        NodeData::Text(text) => !text.trim().is_empty(),
        NodeData::Element(_) => true,
        _ => false,
    };
    let last = document.children(parent).filter(shown).last()?;
    document.element(last).is_some().then_some(last)
}

/// The block among the blocks of `parent` that `node` generated.
fn block_of(parent: &BlockBox, node: NodeId) -> Option<&BlockBox> {
    match &parent.contents {
        BlockContents::Blocks(children) if !crate::multicol::is_multicol(&parent.style) => {
            children.iter().find(|child| child.node == Some(node))
        }
        _ => None,
    }
}

/// The first `<table>` among the open elements below `body`, whose box is `body_box`,
/// with whether the next piece can add rows to it: when it and the elements around it are
/// blocks among blocks, sized by what is around them and not by what is in them, its rows
/// are laid out a piece at a time as they are in the whole document.
fn open_table(
    document: &Document,
    styles: &Styles,
    body: NodeId,
    body_box: &BlockBox,
) -> Option<Result<NodeId, String>> {
    let (mut parent, mut parent_box) = (body, body_box);
    loop {
        let node = open_element(document, parent)?;
        let block = block_of(parent_box, node);
        let is_table = document
            .element(node)
            .is_some_and(|e| e.is_html(&local_name!("table")));
        let style = styles.get(node);
        // A table keeps its columns, so its own width is that of its first rows.
        let sized = !is_table && style.is_some_and(|s| sized_by_content(&s.width));
        let aside = style.is_some_and(|s| {
            s.float != Float::None
                || matches!(
                    s.position,
                    Position::Absolute | Position::Fixed | Position::Running(_)
                )
        });
        let reason = match block {
            _ if sized => within(
                document,
                node,
                "its width is the width of its content, which the rows decide",
            ),
            _ if aside => within(
                document,
                node,
                "it is a float or a positioned box, whose place and size depend on its content",
            ),
            None => not_a_block(document, styles, node, parent_box),
            Some(block) if is_table => match block.contents {
                BlockContents::Table(_) => return Some(Ok(node)),
                _ => not_a_block(document, styles, node, parent_box),
            },
            Some(block) => {
                (parent, parent_box) = (node, block);
                continue;
            }
        };
        // The table to continue is the first one: one inside it is in a cell.
        return first_table_below(document, node).map(|_| Err(reason));
    }
}

/// Whether a width is the size of the box's content.
fn sized_by_content<T>(width: &Size<T>) -> bool {
    matches!(
        width,
        Size::MinContent | Size::MaxContent | Size::FitContent(_) | Size::CalcSize(_)
    )
}

/// Why the box of `node`, a child of the element whose box is `parent`, is not one that a
/// table to continue can be in: the parent lays its content out in another way than as
/// blocks, or the box of `node` is not a block among them.
fn not_a_block(document: &Document, styles: &Styles, node: NodeId, parent: &BlockBox) -> String {
    let blocks = matches!(parent.contents, BlockContents::Blocks(_))
        && !crate::multicol::is_multicol(&parent.style);
    let (node, display) = match parent.node {
        Some(parent) if !blocks => (parent, styles.get(parent).map(|style| style.display)),
        _ => (node, styles.get(node).map(|style| style.display)),
    };
    let reason = match display {
        None | Some(Display::None) => "it is not displayed".to_owned(),
        Some(display) if blocks => format!(
            "its box is not a block among blocks (display: {}, or a float, a positioned box \
             or an inline one)",
            Display::to_css(&display)
        ),
        Some(display) => format!(
            "lays out its content by its size (display: {}, or columns)",
            Display::to_css(&display)
        ),
    };
    within(document, node, &reason)
}

fn within(document: &Document, node: NodeId, reason: &str) -> String {
    let name = document
        .element(node)
        .map_or("?", |element| &*element.name.local);
    format!("the table to continue is in a <{name}> and {reason}")
}

/// The first `<table>` among the open elements from `node` down.
fn first_table_below(document: &Document, node: NodeId) -> Option<NodeId> {
    std::iter::successors(Some(node), |&n| open_element(document, n)).find(|&n| {
        document
            .element(n)
            .is_some_and(|e| e.is_html(&local_name!("table")))
    })
}

fn body_element(document: &Document) -> Option<NodeId> {
    let html = document.document_element()?;
    document.element_children(html).find(|&child| {
        document
            .element(child)
            .is_some_and(|e| e.is_html(&local_name!("body")))
    })
}

/// The box of `<body>` among the children of the root, and which child it is.
fn body_box(root: &BlockBox, body: NodeId) -> Option<(usize, &BlockBox)> {
    let BlockContents::Blocks(children) = &root.contents else {
        return None;
    };
    let at = children.iter().position(|c| c.node == Some(body))?;
    Some((at, &children[at]))
}

fn body_box_mut(root: &mut BlockBox, body: NodeId) -> Option<(usize, &mut BlockBox)> {
    let BlockContents::Blocks(children) = &mut root.contents else {
        return None;
    };
    let at = children.iter().position(|c| c.node == Some(body))?;
    Some((at, &mut children[at]))
}

/// How many of `pages` a flow keeps: those up to the page that the last element of the
/// document with a box starts on, by `order`, the place of each node in the document. It
/// is looked for from the last page back, as far as the pages have it or later elements.
pub(crate) fn kept_pages(order: &HashMap<NodeId, usize>, pages: &[crate::LaidPage]) -> usize {
    let mut last: Option<(usize, usize)> = None;
    for (index, page) in pages.iter().enumerate().rev() {
        let on_page: Vec<usize> = page
            .root
            .iter()
            .flat_map(|root| root.element_boxes())
            .filter_map(|(node, ..)| order.get(&node).copied())
            .collect();
        let most = on_page.iter().max().copied();
        last = match (last, most) {
            (Some((best, _)), Some(most)) if most > best => Some((most, index)),
            (None, Some(most)) => Some((most, index)),
            (Some((best, _)), _) if on_page.contains(&best) => Some((best, index)),
            (Some(_), _) => break,
            (None, None) => None,
        };
    }
    last.map_or(pages.len(), |(_, page)| page + 1)
}

/// What a flow keeps of `document`, whose box tree is `root`, when its open page starts
/// at `token` and is the last of `pages` that it keeps, and `flow_pages` pages of the flow
/// come before that page.
pub(crate) fn open(
    document: &Document,
    styles: &Styles,
    root: &BlockBox,
    before: Option<&Carried>,
    (token, pages): (Option<BreakToken>, usize),
    flow_pages: usize,
) -> Open {
    let mut open = Open {
        carried: Carried {
            start: Start { token, flow_pages },
            anchor: None,
            frozen: Vec::new(),
            table: None,
        },
        removals: Vec::new(),
        pages,
    };
    let Some(body) = body_element(document) else {
        return open;
    };
    let Some((at, body_box)) = body_box(root, body) else {
        return open;
    };
    keep_columns(body_box, before, &mut open.carried.frozen);
    open.carried.table = open_table(document, styles, body, body_box);
    // The open page starts in `<body>`.
    let Some(Resume::Child {
        index,
        token: Some(inner),
    }) = open.carried.start.token.as_ref().map(|t| &t.resume)
    else {
        return open;
    };
    if *index != at {
        return open;
    }
    let done = Done { document, styles };
    let mut anchor = Anchor {
        path: vec![body],
        row: None,
    };
    let (mut parent, mut token) = (body_box, inner.as_ref());
    // At each level, the open page starts in a child: what comes before that child is
    // done.
    while let Resume::Child {
        index,
        token: inner,
    } = &token.resume
    {
        let nested = anchor.path.len() > 1;
        let children = match &parent.contents {
            BlockContents::Blocks(_) if nested && crate::multicol::is_multicol(&parent.style) => {
                break;
            }
            BlockContents::Blocks(children) => children,
            _ => break,
        };
        let Some(child) = children.get(*index) else {
            break;
        };
        let Some(&within) = anchor.path.last() else {
            break;
        };
        let Some(node) = child
            .node
            .filter(|&n| document.node(n).parent() == Some(within))
        else {
            break;
        };
        open.removals.push(done.before(within, node));
        anchor.path.push(node);
        // It starts at a row of a table: the rows before the one above that row are done.
        let row = match (&child.contents, inner.as_deref().map(|t| &t.resume)) {
            (
                BlockContents::Table(table),
                Some(Resume::Row { index } | Resume::RowPart { index, .. }),
            ) => Some((table, *index)),
            _ => None,
        };
        if let Some((table, index)) = row {
            let rows: Vec<&crate::table::Row> = table.body_rows().collect();
            if let Some(node) = rows.get(index).and_then(|row| row.node) {
                anchor.row = Some(node);
                open.removals
                    .extend(done.rows(&rows[..rows_done(&rows, index)]));
            }
            break;
        }
        match inner {
            Some(inner) => (parent, token) = (child, inner),
            None => break,
        }
    }
    if anchor.path.len() > 1 {
        open.carried.anchor = Some(anchor);
    }
    open
}

/// Keeps the columns of the tables in the blocks of `parent`, those they were first laid
/// out with.
fn keep_columns(
    parent: &BlockBox,
    before: Option<&Carried>,
    frozen: &mut Vec<(NodeId, Arc<Frozen>)>,
) {
    let BlockContents::Blocks(children) = &parent.contents else {
        return;
    };
    for child in children {
        let (Some(node), BlockContents::Table(table)) = (child.node, &child.contents) else {
            keep_columns(child, before, frozen);
            continue;
        };
        let kept = before.and_then(|b| b.frozen.iter().find(|(n, _)| *n == node));
        let columns = match kept {
            Some((_, columns)) => Some(columns.clone()),
            None => table.laid_columns().map(Arc::new),
        };
        frozen.extend(columns.map(|columns| (node, columns)));
    }
}

/// How many of `rows`, the body rows of a table whose open page starts at row `index`, are
/// done: a row is kept above the first of the page, which the borders and the sibling
/// selectors of that one look at, and with it the rows whose cells span into the rows
/// that are kept.
fn rows_done(rows: &[&crate::table::Row], index: usize) -> usize {
    let mut end = index.saturating_sub(1);
    while let Some(spanning) = (0..end).find(|&r| {
        let span = rows[r].cells.iter().map(|c| c.rowspan).max().unwrap_or(1);
        r + span > end
    }) {
        end = spanning;
    }
    end
}

impl Removal {
    fn of(parent: NodeId) -> Self {
        Self {
            parent,
            nodes: Vec::new(),
            elements: 0,
            counters: Vec::new(),
        }
    }

    fn take(&mut self, element: NodeId, counters: Vec<(String, i32)>) {
        self.nodes.push(element);
        self.elements += 1;
        for (name, value) in counters {
            match self.counters.iter_mut().find(|(n, _)| *n == name) {
                Some((_, sum)) => *sum = sum.saturating_add(value),
                None => self.counters.push((name, value)),
            }
        }
    }
}

/// Finds what the pages that are written have done with.
struct Done<'a> {
    document: &'a Document,
    styles: &'a Styles,
}

impl Done<'_> {
    /// The removal of the children of `parent` before `node`: the text, and the elements
    /// that can be let go of.
    fn before(&self, parent: NodeId, node: NodeId) -> Removal {
        let mut removal = Removal::of(parent);
        for sibling in self.document.children(parent).take_while(|&s| s != node) {
            if self.document.element(sibling).is_none() {
                removal.nodes.push(sibling);
            } else if let Some(counters) = self.counters_of(sibling) {
                removal.take(sibling, counters);
            }
        }
        removal
    }

    /// What `element` and its descendants add to counters, if the element can be let go
    /// of: nothing in it is shown on later pages, as fixed and running elements are, or
    /// styles them, and all it does to counters is add to them.
    fn counters_of(&self, element: NodeId) -> Option<Vec<(String, i32)>> {
        let mut counters: Vec<(String, i32)> = Vec::new();
        let mut hidden: Option<NodeId> = None;
        for node in self.document.descendants(element) {
            let Some(data) = self.document.element(node) else {
                continue;
            };
            if data.is_html(&local_name!("style")) || data.is_html(&local_name!("link")) {
                return None;
            }
            if let Some(root) = hidden {
                if self.document.ancestors(node).any(|a| a == root) {
                    continue;
                }
                hidden = None;
            }
            let Some(style) = self.styles.get(node) else {
                continue;
            };
            // Elements without boxes do not affect counters.
            if style.display == Display::None {
                hidden = Some(node);
                continue;
            }
            let stays = matches!(style.position, Position::Fixed | Position::Running(_));
            let sets = style.counter_reset.iter().next().is_some()
                || style.counter_set.iter().next().is_some();
            if stays || sets || boxes::is_list_item(style) {
                return None;
            }
            for change in style.counter_increment.iter() {
                let value = change.value.unwrap_or(1);
                match counters.iter_mut().find(|(n, _)| **n == *change.name) {
                    Some((_, sum)) => *sum = sum.saturating_add(value),
                    None => counters.push((change.name.to_string(), value)),
                }
            }
        }
        Some(counters)
    }

    /// The removals of `rows`, the first rows of a table's body: as many of them as can
    /// be let go of, each with the text before it, from the start of its row group.
    fn rows(&self, rows: &[&crate::table::Row]) -> Vec<Removal> {
        let mut removals: Vec<Removal> = Vec::new();
        let mut removed: HashSet<NodeId> = HashSet::new();
        for row in rows {
            let Some(node) = row.node else {
                break;
            };
            let Some(parent) = self.document.node(node).parent() else {
                break;
            };
            let first = self
                .document
                .previous_element_sibling(node)
                .is_none_or(|previous| removed.contains(&previous));
            let Some(counters) = self.counters_of(node).filter(|_| first) else {
                break;
            };
            if removals.last().is_none_or(|r| r.parent != parent) {
                removals.push(Removal::of(parent));
            }
            let Some(removal) = removals.last_mut() else {
                break;
            };
            let before = std::iter::successors(self.document.node(node).previous_sibling(), |&s| {
                self.document.node(s).previous_sibling()
            });
            // What is before the row that was let go of before it is in a removal already.
            removal
                .nodes
                .extend(before.take_while(|s| !removed.contains(s)));
            removal.take(node, counters);
            removed.insert(node);
        }
        removals
    }
}

/// Lets go of what the pages before the open page of a flow have done with, and returns
/// what the flow keeps for its next piece, with `document`.
pub fn let_go(document: &mut Document, open: Open) -> Carried {
    let Open {
        mut carried,
        removals,
        ..
    } = open;
    if removals.iter().all(|removal| removal.nodes.is_empty()) {
        return carried;
    }
    for removal in &removals {
        for &node in &removal.nodes {
            document.remove(node);
        }
        document.note_removed(removal.parent, removal.elements, &removal.counters);
    }
    let (compact, map) = document.compacted();
    *document = compact;
    let moved = |node: NodeId| map.get(&node).copied();
    carried.anchor = carried.anchor.and_then(|anchor| {
        Some(Anchor {
            path: anchor.path.into_iter().map(moved).collect::<Option<_>>()?,
            row: match anchor.row {
                Some(row) => Some(moved(row)?),
                None => None,
            },
        })
    });
    carried.table = carried.table.and_then(|table| match table {
        Ok(node) => moved(node).map(Ok),
        Err(reason) => Some(Err(reason)),
    });
    carried.frozen = carried
        .frozen
        .into_iter()
        .filter_map(|(node, frozen)| Some((moved(node)?, frozen)))
        .collect();
    carried
}
