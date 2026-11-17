//! Changing a parsed tree: what a flow printed a piece at a time needs, to add the next
//! piece to the document and to let go of what is printed.

use std::collections::HashMap;

use crate::{Document, NodeData, NodeId};

/// What was removed from the start of an element's children: what the children that are
/// left still need to know of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removed {
    /// How many elements were removed, which `:nth-child()` of those left counts.
    pub elements: u32,
    /// The sum of what the removed elements added to each counter.
    pub counters: Vec<(String, i32)>,
}

impl Document {
    /// Takes `node` and its descendants out of the tree. Their place in the arena stays
    /// taken until the document is [compacted](Self::compacted).
    pub fn remove(&mut self, node: NodeId) {
        self.detach(node);
    }

    /// Notes that `elements` elements were removed from the start of the children of
    /// `parent`, and what they added to counters.
    pub fn note_removed(&mut self, parent: NodeId, elements: u32, counters: &[(String, i32)]) {
        let removed = self.removed.entry(parent).or_default();
        removed.elements += elements;
        for (name, value) in counters {
            match removed.counters.iter_mut().find(|(n, _)| n == name) {
                Some((_, sum)) => *sum = sum.saturating_add(*value),
                None => removed.counters.push((name.clone(), *value)),
            }
        }
    }

    /// What was removed from the start of the children of `parent`, if anything was.
    pub fn removed_before(&self, parent: NodeId) -> Option<&Removed> {
        self.removed.get(&parent)
    }

    /// How many elements were removed before the element `node` among its siblings.
    pub fn removed_siblings(&self, node: NodeId) -> usize {
        if self.removed.is_empty() {
            return 0;
        }
        let parent = self.node(node).parent;
        parent
            .and_then(|parent| self.removed.get(&parent))
            .map_or(0, |removed| removed.elements as usize)
    }

    /// Copies `node` of `other`, with its descendants, to the end of the children of
    /// `parent`, and returns the copy.
    pub fn adopt(&mut self, parent: NodeId, other: &Document, node: NodeId) -> NodeId {
        let copy = self.copy(other, node, &mut None);
        self.append(parent, copy);
        copy
    }

    /// The document with only the nodes that are in its tree, and where each of them is
    /// in it.
    pub fn compacted(&self) -> (Document, HashMap<NodeId, NodeId>) {
        let mut compact = Document::empty(self.quirks_mode);
        let mut map = Some(HashMap::from([(self.root(), compact.root())]));
        let children: Vec<NodeId> = self.children(self.root()).collect();
        for child in children {
            let copy = compact.copy(self, child, &mut map);
            let root = compact.root();
            compact.append(root, copy);
        }
        let map = map.unwrap_or_default();
        for (parent, removed) in &self.removed {
            if let Some(parent) = map.get(parent) {
                compact.removed.insert(*parent, removed.clone());
            }
        }
        (compact, map)
    }

    /// A copy of `node` of `other` and its descendants, not in the tree yet.
    fn copy(
        &mut self,
        other: &Document,
        node: NodeId,
        map: &mut Option<HashMap<NodeId, NodeId>>,
    ) -> NodeId {
        let mut data = other.node(node).data.clone();
        if let NodeData::Element(element) = &mut data
            && let Some(contents) = element.template_contents
        {
            element.template_contents = Some(self.copy(other, contents, map));
        }
        let copy = self.push(data);
        if let Some(map) = map {
            map.insert(node, copy);
        }
        let mut child = other.node(node).first_child;
        while let Some(next) = child {
            let copied = self.copy(other, next, map);
            self.append(copy, copied);
            child = other.node(next).next_sibling;
        }
        copy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_name;

    fn texts(document: &Document) -> String {
        document.text_content(document.root())
    }

    #[test]
    fn adopts_removes_and_compacts() {
        let mut document = Document::parse("<table><tbody><tr><td>a<tr><td>b</table>");
        let more = Document::parse("<table><tbody><tr><td>c</table><p>d</p>");
        let body = document.find_html(&local_name!("tbody")).unwrap();
        let row = more.find_html(&local_name!("tr")).unwrap();
        document.adopt(body, &more, row);
        assert_eq!(texts(&document), "abc");
        let first = document.find_html(&local_name!("tr")).unwrap();
        document.remove(first);
        document.note_removed(body, 1, &[("row".into(), 1)]);
        document.note_removed(body, 1, &[("row".into(), 2)]);
        assert_eq!(texts(&document), "bc");
        let before = document.node_count();
        let (compact, map) = document.compacted();
        assert_eq!(texts(&compact), "bc");
        assert_eq!(compact.node_count(), before - 3);
        let removed = compact.removed_before(map[&body]).unwrap();
        assert_eq!(removed.elements, 2);
        assert_eq!(removed.counters, [("row".to_owned(), 3)]);
        let kept = compact.find_html(&local_name!("tr")).unwrap();
        assert_eq!(compact.removed_siblings(kept), 2);
    }
}
