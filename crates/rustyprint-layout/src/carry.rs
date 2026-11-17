//! Flows laid out a piece at a time: what is kept of a document between two pieces, and
//! what is let go of.
//!
//! The last page of a flow is open: the next piece may put content on it. The page is laid
//! out again, with that content, from the place in the box tree where it starts, so what is
//! kept is that place and the part of the document from there on. The place is a
//! [`BreakToken`], which counts boxes from the start of their parent; the nodes that those
//! counts stood for are kept with it, so that the counts can be found again once the
//! content before them is gone.

use std::collections::HashSet;
use std::sync::Arc;

use rustyprint_css::Styles;
use rustyprint_css::properties::types::{Display, Position};
use rustyprint_dom::{Document, NodeId, local_name};

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
    /// The columns of the tables that are children of `<body>`, as they were first laid
    /// out: the rows a table has let go of, and those it has yet to get, do not change them.
    frozen: Vec<(NodeId, Arc<Frozen>)>,
}

/// The nodes of the boxes that a token counts to: `<body>` in the root, the child of
/// `<body>` that the open page starts in, and the row of that child, if it is a table that
/// the page starts in the rows of.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anchor {
    body: NodeId,
    child: NodeId,
    row: Option<NodeId>,
}

/// The open page of a flow that was just laid out: what to keep of it, and the nodes of
/// its document that the pages before the open one have done with.
#[derive(Debug)]
pub struct Open {
    carried: Carried,
    removals: Vec<Removal>,
}

/// Nodes to remove from the start of the children of `parent`.
#[derive(Debug)]
struct Removal {
    parent: NodeId,
    nodes: Vec<NodeId>,
    elements: u32,
    counters: Vec<(String, i32)>,
}

impl Carried {
    /// How many pages of the flow come before its open page.
    pub fn flow_pages(&self) -> usize {
        self.start.flow_pages
    }

    /// The last table among the children of `<body>` that has its columns kept, if
    /// nothing but white space follows it: the table that the next piece can add rows to.
    pub fn last_table(&self, document: &Document) -> Option<NodeId> {
        let body = body_element(document)?;
        let shown = |child: &NodeId| match document.node(*child).data() {
            rustyprint_dom::NodeData::Text(text) => !text.trim().is_empty(),
            rustyprint_dom::NodeData::Element(_) => true,
            _ => false,
        };
        let last = document.children(body).filter(shown).last()?;
        self.frozen
            .iter()
            .any(|(node, _)| *node == last)
            .then_some(last)
    }

    /// Where in the document the element is that the open page starts in, for the margin
    /// boxes: what comes before it is on earlier pages.
    pub(crate) fn starts_in(&self) -> Option<NodeId> {
        self.anchor.map(|anchor| anchor.child)
    }

    /// Gives the tables of `root` the columns they keep, and finds where the open page
    /// starts in `root`, the box tree of the flow's document as it is now.
    pub(crate) fn apply(&self, document: &Document, root: &mut BlockBox) -> Result<Start, String> {
        if !self.frozen.is_empty() {
            let children = body_element(document)
                .and_then(|body| body_box_mut(root, body))
                .and_then(|(_, body)| match &mut body.contents {
                    BlockContents::Blocks(children) => Some(children),
                    _ => None,
                });
            for child in children.into_iter().flatten() {
                let BlockContents::Table(table) = &mut child.contents else {
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
        }
        let mut start = self.start.clone();
        if let (Some(token), Some(anchor)) = (&mut start.token, self.anchor) {
            rebase(token, anchor, root)
                .ok_or("the page a piece continues on is not in the document any more")?;
        }
        Ok(start)
    }
}

const GONE: Option<()> = None;

/// Sets the counts of `token` to where the boxes of `anchor` are in `root`.
fn rebase(token: &mut BreakToken, anchor: Anchor, root: &BlockBox) -> Option<()> {
    let (at, body) = body_box(root, anchor.body)?;
    let Resume::Child { index, token } = &mut token.resume else {
        return GONE;
    };
    *index = at;
    let BlockContents::Blocks(children) = &body.contents else {
        return GONE;
    };
    let at = children
        .iter()
        .position(|child| child.node == Some(anchor.child))?;
    let Resume::Child { index, token } = &mut token.as_mut()?.resume else {
        return GONE;
    };
    *index = at;
    let Some(row) = anchor.row else {
        return Some(());
    };
    let BlockContents::Table(table) = &children[at].contents else {
        return GONE;
    };
    let at = table.body_rows().position(|r| r.node == Some(row))?;
    match &mut token.as_mut()?.resume {
        Resume::Row { index } | Resume::RowPart { index, .. } => *index = at,
        _ => return GONE,
    }
    Some(())
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

/// What a flow keeps of `document`, whose box tree is `root`, when its last page starts
/// at `token` and `flow_pages` pages of the flow come before that page.
pub(crate) fn open(
    document: &Document,
    styles: &Styles,
    root: &BlockBox,
    before: Option<&Carried>,
    token: Option<BreakToken>,
    flow_pages: usize,
) -> Open {
    let mut open = Open {
        carried: Carried {
            start: Start { token, flow_pages },
            anchor: None,
            frozen: Vec::new(),
        },
        removals: Vec::new(),
    };
    let Some(body) = body_element(document) else {
        return open;
    };
    let Some((at, body_box)) = body_box(root, body) else {
        return open;
    };
    let BlockContents::Blocks(children) = &body_box.contents else {
        return open;
    };
    for child in children {
        if let (Some(node), BlockContents::Table(table)) = (child.node, &child.contents) {
            let kept = before.and_then(|b| b.frozen.iter().find(|(n, _)| *n == node));
            let frozen = match kept {
                Some((_, frozen)) => Some(frozen.clone()),
                None => table.laid_columns().map(Arc::new),
            };
            open.carried.frozen.extend(frozen.map(|f| (node, f)));
        }
    }
    // The open page starts in a child of `<body>`: what comes before that child is done.
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
    let Resume::Child { index, token } = &inner.resume else {
        return open;
    };
    let Some(child) = children.get(*index) else {
        return open;
    };
    let Some(node) = child
        .node
        .filter(|&n| document.node(n).parent() == Some(body))
    else {
        return open;
    };
    let mut anchor = Anchor {
        body,
        child: node,
        row: None,
    };
    let done = Done { document, styles };
    let mut removal = Removal::of(body);
    for sibling in document.children(body).take_while(|&s| s != node) {
        if document.element(sibling).is_none() {
            removal.nodes.push(sibling);
        } else if let Some(counters) = done.counters_of(sibling) {
            removal.take(sibling, counters);
        }
    }
    open.removals.push(removal);
    // It starts at a row of a table: the rows before the one above that row are done.
    let row = match (&child.contents, token.as_deref().map(|t| &t.resume)) {
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
            // A row is kept above the first of the page, which the borders and the
            // sibling selectors of that one look at, and with it the rows whose cells
            // span into the rows that are kept.
            let mut end = index.saturating_sub(1);
            while let Some(spanning) = (0..end).find(|&r| {
                let span = rows[r].cells.iter().map(|c| c.rowspan).max().unwrap_or(1);
                r + span > end
            }) {
                end = spanning;
            }
            open.removals.extend(done.rows(&rows[..end]));
        }
    }
    open.carried.anchor = Some(anchor);
    open
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
            removal
                .nodes
                .extend(before.filter(|s| !removed.contains(s)));
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
            body: moved(anchor.body)?,
            child: moved(anchor.child)?,
            row: match anchor.row {
                Some(row) => Some(moved(row)?),
                None => None,
            },
        })
    });
    carried.frozen = carried
        .frozen
        .into_iter()
        .filter_map(|(node, frozen)| Some((moved(node)?, frozen)))
        .collect();
    carried
}
