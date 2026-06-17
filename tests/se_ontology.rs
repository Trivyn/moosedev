//! Confirms the content-agnostic loader handles the *generated* software-
//! engineering ontology (Trivyn output): RDF-star alignment annotations,
//! `owl:imports`, and slash-based namespacing all parse, and MOOSE's
//! `extract_compact_vocabulary` surfaces the clean domain classes + relations.

use std::path::Path;

use moosedev::ontology;
use oxigraph::model::NamedNodeRef;
use oxigraph::store::Store;

const SE_GRAPH: &str = ontology::SE_DOMAIN_GRAPH_IRI;
const SE_SHAPES_GRAPH: &str = ontology::SE_SHAPES_GRAPH_IRI;

#[test]
fn loads_generated_software_engineering_ontology() {
    let store = Store::new().unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("ontologies")
        .join(ontology::SE_DOMAIN_TTL);

    ontology::load_turtle(&store, &path, SE_GRAPH)
        .expect("generated SE ontology (with RDF-star annotations) should parse");

    let vocab = moose::vocabulary::extract_compact_vocabulary(&store, SE_GRAPH, None)
        .expect("extract vocabulary from the generated ontology");

    let locals: Vec<&str> = vocab
        .classes
        .iter()
        .map(|c| c.local_name.as_str())
        .collect();
    for expected in [
        "SoftwareSystem",
        "Subsystem",
        "Component",
        "Service",
        "Module",
        "Interface",
        "DataStore",
    ] {
        assert!(
            locals.contains(&expected),
            "expected class {expected} in extracted vocab; got {locals:?}"
        );
    }

    assert!(
        vocab.object_properties.len() >= 3,
        "expected SE relations (dependsOn, implements, …); got {}",
        vocab.object_properties.len()
    );
}

#[test]
fn loads_generated_software_engineering_shapes() {
    let store = Store::new().unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("ontologies")
        .join(ontology::SE_SHAPES_TTL);
    ontology::load_turtle(&store, &path, SE_SHAPES_GRAPH).expect("SHACL shapes should parse");

    let node_shapes = store
        .quads_for_pattern(
            None,
            Some(NamedNodeRef::new(moose::RDF_TYPE).unwrap()),
            Some(
                NamedNodeRef::new("http://www.w3.org/ns/shacl#NodeShape")
                    .unwrap()
                    .into(),
            ),
            None,
        )
        .count();
    assert!(
        node_shapes >= 5,
        "expected several sh:NodeShape; got {node_shapes}"
    );
}
