#![allow(dead_code)]

use renamite_model::{Document, Node, NodeId, NodeKind};

pub fn collect(doc: &Document, id: NodeId, pred: &impl Fn(&Node) -> bool, out: &mut Vec<NodeId>) {
    let node = &doc.nodes[id];
    if pred(node) {
        out.push(id);
    }
    for &child in &node.children {
        collect(doc, child, pred, out);
    }
}

pub fn find_all(doc: &Document, pred: impl Fn(&Node) -> bool) -> Vec<NodeId> {
    let mut out = Vec::new();
    let comp = &doc.compositions[doc.main];
    for &root in &comp.children {
        collect(doc, root, &pred, &mut out);
    }
    out
}

pub fn paths(doc: &Document) -> Vec<NodeId> {
    find_all(doc, |n| matches!(n.kind, NodeKind::Shape(_)))
}

pub fn fills(doc: &Document) -> Vec<NodeId> {
    find_all(doc, |n| matches!(&n.kind, NodeKind::Style(_)))
}
