//! Relation catalogue construction and SHACL domain/range validation.
//! This is the single in-memory source of truth for legal record-to-record edges.

use oxigraph::model::NamedNode;
use oxigraph::sparql::QueryResults;
use oxigraph::store::Store;

use crate::ontology;

use super::capture::{asserted_project_types, require_information_record};
#[cfg(test)]
use super::context::PRIORITY_EDGES;
use super::state::AppState;
use super::util::{
    any_subclass_of, class_list, iri_value, is_subclass_of, local_name, run_sparql, unique_classes,
    RDF_FIRST, RDF_REST, SH_CLASS, SH_OR, SH_PATH, SH_PROPERTY, SH_TARGET_CLASS,
};

#[derive(Debug, Clone)]
struct RelationConstraint {
    subject_class: String,
    object_class: String,
}

/// A single SHACL-declared object-property constraint: `predicate_iri` admits a
/// subject of `subject_class` (the shape's `sh:targetClass`, the domain) pointing
/// at an object of `object_class` (the property branch's `sh:class`, the range).
#[derive(Debug, Clone)]
struct CatalogEntry {
    predicate_iri: String,
    predicate_local: String,
    subject_class: String,
    object_class: String,
}

/// Direction a legal edge must run for an ordered class pair `(a, b)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDirection {
    /// `a --predicate--> b` (a is the domain/subject).
    Forward,
    /// `b --predicate--> a` (b is the domain/subject).
    Inverse,
}

/// An object property legal between a class pair, with the direction it runs.
#[derive(Debug, Clone)]
pub struct LegalEdge {
    pub predicate_local: String,
    pub predicate_iri: String,
    pub direction: EdgeDirection,
}

/// The project's object-property domain/range table, read once from the loaded
/// SHACL shape graphs at bootstrap (shapes are static post-load, so it never needs
/// invalidation). The single in-memory source of truth for relation legality:
/// `relate` and inline capture validate against it, and the link-suggester
/// enumerates candidate predicates from it — replacing per-call SPARQL.
#[derive(Debug, Clone, Default)]
pub struct RelationCatalogue {
    entries: Vec<CatalogEntry>,
}

impl RelationCatalogue {
    /// Domain/range constraints declared for one predicate IRI — the subject/object
    /// class pairs its shapes permit. Replaces the old per-call shapes query.
    fn constraints_for_predicate(&self, predicate_iri: &str) -> Vec<RelationConstraint> {
        self.entries
            .iter()
            .filter(|e| e.predicate_iri == predicate_iri)
            .map(|e| RelationConstraint {
                subject_class: e.subject_class.clone(),
                object_class: e.object_class.clone(),
            })
            .collect()
    }

    /// Object classes expected for a predicate's object endpoint, as declared by
    /// SHACL. Used by inline capture to decide when a target may be a typed
    /// non-record instance such as `SystemComponent`.
    pub(crate) fn expected_object_classes(&self, predicate_iri: &str) -> Vec<String> {
        unique_classes(
            self.entries
                .iter()
                .filter(|e| e.predicate_iri == predicate_iri)
                .map(|e| e.object_class.clone()),
        )
    }

    /// Object properties legal between two record classes, in either direction,
    /// subclass-aware (so `InformationRecord`-level constraints apply to every
    /// pair). `Forward` ⇒ the edge is `a --pred--> b`; `Inverse` ⇒ `b --pred--> a`.
    /// Deduplicated by (predicate, direction).
    pub fn legal_predicates(&self, store: &Store, a_class: &str, b_class: &str) -> Vec<LegalEdge> {
        let mut out: Vec<LegalEdge> = Vec::new();
        for entry in &self.entries {
            let forward = is_subclass_of(store, a_class, &entry.subject_class)
                && is_subclass_of(store, b_class, &entry.object_class);
            let inverse = is_subclass_of(store, b_class, &entry.subject_class)
                && is_subclass_of(store, a_class, &entry.object_class);
            for (matched, direction) in [
                (forward, EdgeDirection::Forward),
                (inverse, EdgeDirection::Inverse),
            ] {
                if matched
                    && !out
                        .iter()
                        .any(|e| e.predicate_iri == entry.predicate_iri && e.direction == direction)
                {
                    out.push(LegalEdge {
                        predicate_local: entry.predicate_local.clone(),
                        predicate_iri: entry.predicate_iri.clone(),
                        direction,
                    });
                }
            }
        }
        out
    }

    /// Local names of every object property in the catalogue (for drift checks).
    #[cfg(test)]
    fn predicate_locals(&self) -> std::collections::HashSet<&str> {
        self.entries
            .iter()
            .map(|e| e.predicate_local.as_str())
            .collect()
    }
}

/// Read every object-property constraint from the loaded SHACL shape graphs into a
/// [`RelationCatalogue`]. Generalizes the old per-predicate query: it binds
/// `?predicate` instead of fixing one, and keeps only property branches declaring
/// an `sh:class` (range) — `sh:datatype` branches drop out — so the table is
/// exactly the record→record object-property vocabulary, including `sh:or` union
/// ranges (e.g. `isMotivatedBy` → Constraint|Requirement). Union ranges may be
/// declared either by the legacy node-level `sh:or` branch shape or by a proper
/// property-level `sh:or`.
pub(crate) fn build_relation_catalogue(store: &Store) -> RelationCatalogue {
    let sparql = format!(
        r#"
SELECT DISTINCT ?predicate ?subjectClass ?objectClass
WHERE {{
  VALUES ?shapeGraph {{ <{}> <{}> }}
  GRAPH ?shapeGraph {{
    ?shape <{}> ?subjectClass .
    {{
      ?shape <{}> ?propertyShape .
      ?propertyShape <{}> ?predicate ;
                     <{}> ?objectClass .
    }} UNION {{
      ?shape <{}>/<{}>*/<{}> ?propertyShape .
      ?propertyShape <{}> ?predicate ;
                     <{}> ?objectClass .
    }} UNION {{
      ?shape <{}> ?propertyShape .
      ?propertyShape <{}> ?predicate ;
                     <{}>/<{}>*/<{}> ?branch .
      ?branch <{}> ?objectClass .
    }}
  }}
}}"#,
        ontology::SE_SHAPES_GRAPH_IRI,
        ontology::ARCH_SHAPES_GRAPH_IRI,
        SH_TARGET_CLASS,
        SH_PROPERTY,
        SH_PATH,
        SH_CLASS,
        SH_OR,
        RDF_REST,
        RDF_FIRST,
        SH_PATH,
        SH_CLASS,
        SH_PROPERTY,
        SH_PATH,
        SH_OR,
        RDF_REST,
        RDF_FIRST,
        SH_CLASS
    );

    let Ok(QueryResults::Solutions(solutions)) = run_sparql(store, &sparql) else {
        return RelationCatalogue::default();
    };
    let entries = solutions
        .flatten()
        .filter_map(|solution| {
            let predicate_iri = iri_value(solution.get("predicate"))?;
            let predicate_local = local_name(&predicate_iri).to_string();
            Some(CatalogEntry {
                predicate_iri,
                predicate_local,
                subject_class: iri_value(solution.get("subjectClass"))?,
                object_class: iri_value(solution.get("objectClass"))?,
            })
        })
        .collect();
    RelationCatalogue { entries }
}

/// Validate a relation against the loaded SHACL shape contract before writing, for
/// an *existing* subject record. Thin wrapper over
/// [`validate_relation_for_subject_types`] that reads the subject's asserted types.
pub(crate) fn validate_relation_endpoints(
    state: &AppState,
    subject: &NamedNode,
    predicate_iri: &str,
    object: &NamedNode,
) -> anyhow::Result<()> {
    let subject_types = asserted_project_types(state, subject);
    validate_relation_for_subject_types(
        state,
        &subject_types,
        subject.as_str(),
        predicate_iri,
        object,
    )
}

/// Validate a relation against the SHACL contract using *supplied* subject types,
/// so it works before the subject is minted (inline capture) as well as for an
/// existing subject (`relate`). `subject_desc` labels the subject in error
/// messages. The object must already exist in the project graph. If the predicate
/// has no object constraint in the shapes, preserve the legacy safe default: the
/// subject's types must include an `InformationRecord` and the object must be one.
pub(crate) fn validate_relation_for_subject_types(
    state: &AppState,
    subject_types: &[String],
    subject_desc: &str,
    predicate_iri: &str,
    object: &NamedNode,
) -> anyhow::Result<()> {
    let constraints = state.catalogue.constraints_for_predicate(predicate_iri);
    if constraints.is_empty() {
        let info_record = state.resolve_class("InformationRecord")?;
        if !subject_types
            .iter()
            .any(|t| is_subclass_of(&state.store, t, &info_record))
        {
            anyhow::bail!(
                "cannot relate subject {subject_desc}: actual class(es) [{}], expected [InformationRecord]",
                class_list(subject_types)
            );
        }
        require_information_record(state, object)
            .map_err(|e| anyhow::anyhow!("cannot relate object {}: {e}", object.as_str()))?;
        return Ok(());
    }

    let object_types = asserted_project_types(state, object);
    let expected_subjects = unique_classes(
        constraints
            .iter()
            .map(|constraint| constraint.subject_class.clone()),
    );

    let matching_subject_constraints: Vec<&RelationConstraint> = constraints
        .iter()
        .filter(|constraint| {
            any_subclass_of(
                &state.store,
                subject_types,
                std::slice::from_ref(&constraint.subject_class),
            )
        })
        .collect();

    if matching_subject_constraints.is_empty() {
        anyhow::bail!(
            "cannot relate subject {subject_desc}: actual class(es) [{}], expected [{}]",
            class_list(subject_types),
            class_list(&expected_subjects)
        );
    }

    let expected_objects = unique_classes(
        matching_subject_constraints
            .iter()
            .map(|constraint| constraint.object_class.clone()),
    );
    if !any_subclass_of(&state.store, &object_types, &expected_objects) {
        anyhow::bail!(
            "cannot relate object {}: actual class(es) [{}], expected [{}]",
            object.as_str(),
            class_list(&object_types),
            class_list(&expected_objects)
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Architecture-domain class namespace (matches the shipped ontologies).
    const ARCH: &str = "https://trivyn.io/ontologies/software/architecture#";

    /// In-memory store with just the shipped domain + SHACL shape graphs loaded —
    /// enough to build and exercise the relation catalogue.
    fn shapes_store() -> Store {
        let store = Store::new().expect("in-memory store");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ontologies");
        ontology::load_ontologies(&store, &dir).expect("load ontologies");
        store
    }

    fn cls(local: &str) -> String {
        format!("{ARCH}{local}")
    }

    #[test]
    fn catalogue_captures_object_properties_including_union_ranges() {
        let store = shapes_store();
        let cat = build_relation_catalogue(&store);
        let locals = cat.predicate_locals();
        for p in [
            "isMotivatedBy",
            "violates",
            "constrains",
            "learnedFrom",
            "concerns",
            "weighs",
            "resultsIn",
            "supersedes",
            "dependsOn",
        ] {
            assert!(
                locals.contains(p),
                "catalogue missing object property {p:?}"
            );
        }
        // `isMotivatedBy` has a SHACL `sh:or` union range — both branches present.
        let to_req =
            cat.legal_predicates(&store, &cls("ArchitecturalDecision"), &cls("Requirement"));
        let to_con =
            cat.legal_predicates(&store, &cls("ArchitecturalDecision"), &cls("Constraint"));
        assert!(
            to_req
                .iter()
                .any(|e| e.predicate_local == "isMotivatedBy"
                    && e.direction == EdgeDirection::Forward)
        );
        assert!(
            to_con
                .iter()
                .any(|e| e.predicate_local == "isMotivatedBy"
                    && e.direction == EdgeDirection::Forward)
        );
    }

    #[test]
    fn legal_predicates_respects_domain_range_and_direction() {
        let store = shapes_store();
        let cat = build_relation_catalogue(&store);
        // No *semantic* object property links an AntiPattern to a Requirement
        // (`violates` ranges over Constraint, `isMotivatedBy`'s domain is a
        // decision). Only the InformationRecord-level lifecycle predicates
        // (supersedes/isSupersededBy) apply to any record pair — the suggester
        // filters those out.
        let semantic: Vec<_> = cat
            .legal_predicates(&store, &cls("AntiPattern"), &cls("Requirement"))
            .into_iter()
            .filter(|e| {
                !matches!(
                    e.predicate_local.as_str(),
                    "supersedes" | "isSupersededBy" | "hasRationale" | "isRationaleFor"
                )
            })
            .collect();
        assert!(
            semantic.is_empty(),
            "unexpected semantic links: {semantic:?}"
        );
        // From a Requirement to a decision, `isMotivatedBy` is legal but Inverse
        // (the edge runs decision -> requirement).
        let from_req =
            cat.legal_predicates(&store, &cls("Requirement"), &cls("ArchitecturalDecision"));
        assert!(
            from_req
                .iter()
                .any(|e| e.predicate_local == "isMotivatedBy"
                    && e.direction == EdgeDirection::Inverse)
        );
    }

    #[test]
    fn concerns_domain_broadened_to_information_record() {
        let store = shapes_store();
        let cat = build_relation_catalogue(&store);
        for subject in ["Lesson", "Constraint", "ArchitecturalDecision"] {
            let legal = cat.legal_predicates(&store, &cls(subject), &cls("SystemComponent"));
            assert!(
                legal.iter().any(|e| {
                    e.predicate_local == "concerns" && e.direction == EdgeDirection::Forward
                }),
                "{subject} should be allowed to concern a SystemComponent"
            );
        }
        let concerns = format!("{ARCH}concerns");
        assert_eq!(
            cat.expected_object_classes(&concerns),
            vec![cls("SystemComponent")]
        );
    }

    #[test]
    fn priority_edges_are_all_in_catalogue() {
        let store = shapes_store();
        let cat = build_relation_catalogue(&store);
        let locals = cat.predicate_locals();
        for p in PRIORITY_EDGES {
            assert!(
                locals.contains(p),
                "PRIORITY_EDGES lists {p:?} but no SHACL shape declares it (ontology drift)"
            );
        }
    }
}
