use renamite_history::{EditorCommand, History, NodeTree, ProjectMut};
use renamite_machine::{ClipMap, MachineMap};
use renamite_model::{Document, Node, NodeKind, Parent, PropPath, Value};

struct Fixture {
    document: Document,
    clips: ClipMap,
    clip_order: Vec<renamite_machine::ClipId>,
    machines: MachineMap,
    machine_order: Vec<renamite_machine::MachineId>,
    start_machine: Option<renamite_machine::MachineId>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            document: Document::empty(),
            clips: ClipMap::default(),
            clip_order: Vec::new(),
            machines: MachineMap::default(),
            machine_order: Vec::new(),
            start_machine: None,
        }
    }

    fn project(&mut self) -> ProjectMut<'_> {
        ProjectMut {
            document: &mut self.document,
            clips: &mut self.clips,
            clip_order: &mut self.clip_order,
            machines: &mut self.machines,
            machine_order: &mut self.machine_order,
            start_machine: &mut self.start_machine,
        }
    }
}

#[test]
fn coalesced_property_edit_undo_redo_uses_latest_value() {
    let mut fixture = Fixture::new();
    let id = fixture
        .document
        .create_node(Node::new("node", NodeKind::Group));
    let mut history = History::new();

    history.begin("opacity");
    history
        .apply(
            &mut fixture.project(),
            EditorCommand::SetStatic {
                id,
                prop: PropPath::new("opacity"),
                value: Value::F64(0.25),
            },
        )
        .unwrap();
    history
        .apply(
            &mut fixture.project(),
            EditorCommand::SetStatic {
                id,
                prop: PropPath::new("opacity"),
                value: Value::F64(0.75),
            },
        )
        .unwrap();
    history.commit();

    assert_eq!(fixture.document.nodes[id].opacity.base, 0.75);
    history.undo(&mut fixture.project()).unwrap();
    assert_eq!(fixture.document.nodes[id].opacity.base, 1.0);
    history.redo(&mut fixture.project()).unwrap();
    assert_eq!(fixture.document.nodes[id].opacity.base, 0.75);
}

#[test]
fn cancelling_structural_transaction_detaches_created_node() {
    let mut fixture = Fixture::new();
    let main = fixture.document.main;
    let mut history = History::new();
    history.begin("insert");

    let applied = history
        .apply(
            &mut fixture.project(),
            EditorCommand::InsertNode {
                parent: Parent::Comp(main),
                index: 0,
                tree: NodeTree::leaf(Node::new("inserted", NodeKind::Group)),
            },
        )
        .unwrap();
    let id = applied.created.unwrap();
    assert_eq!(fixture.document.compositions[main].children, vec![id]);

    history.cancel(&mut fixture.project()).unwrap();
    assert!(fixture.document.compositions[main].children.is_empty());
    assert!(fixture.document.nodes.contains_key(id));
}

#[test]
fn grouping_undo_restores_original_sibling_order() {
    let mut fixture = Fixture::new();
    let main = fixture.document.main;
    let a = fixture
        .document
        .create_node(Node::new("a", NodeKind::Group));
    let b = fixture
        .document
        .create_node(Node::new("b", NodeKind::Group));
    let c = fixture
        .document
        .create_node(Node::new("c", NodeKind::Group));
    let group = fixture
        .document
        .create_node(Node::new("group", NodeKind::Group));
    for id in [a, b, c, group] {
        fixture
            .document
            .attach(id, Parent::Comp(main), usize::MAX)
            .unwrap();
    }
    let original = fixture.document.compositions[main].children.clone();
    let mut history = History::new();

    history
        .apply(
            &mut fixture.project(),
            EditorCommand::GroupNodes {
                ids: vec![a, c],
                group,
            },
        )
        .unwrap();
    history.commit();
    assert_eq!(fixture.document.nodes[group].children, vec![a, c]);

    history.undo(&mut fixture.project()).unwrap();
    assert_eq!(fixture.document.compositions[main].children, original);
    history.redo(&mut fixture.project()).unwrap();
    assert_eq!(fixture.document.nodes[group].children, vec![a, c]);
}
