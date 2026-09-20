//! Read-only SPARQL access over the local MOOSEDev store.

use oxigraph::io::{RdfFormat, RdfSerializer};
use oxigraph::sparql::results::{QueryResultsFormat, QueryResultsSerializer};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;

/// Run a read-only SPARQL query and serialize the results for MCP clients.
pub fn run_query(store: &Store, query: &str) -> anyhow::Result<String> {
    let mut prepared = SparqlEvaluator::new()
        .parse_query(query)
        .map_err(|e| anyhow::anyhow!("parse query: {e}"))?;
    if prepared.dataset().is_default_dataset() {
        prepared.dataset_mut().set_default_graph_as_union();
    }
    let results = prepared
        .on_store(store)
        .execute()
        .map_err(|e| anyhow::anyhow!("execute query: {e}"))?;
    serialize_results(results)
}

/// Serialize Oxigraph's three query result families into the tool's stable text
/// formats: SPARQL JSON for SELECT/ASK and N-Triples for graph results.
fn serialize_results(results: QueryResults<'_>) -> anyhow::Result<String> {
    let mut out = Vec::new();
    match results {
        QueryResults::Solutions(solutions) => {
            let mut serializer = QueryResultsSerializer::from_format(QueryResultsFormat::Json)
                .serialize_solutions_to_writer(&mut out, solutions.variables().to_vec())?;
            for solution in solutions {
                serializer.serialize(&solution?)?;
            }
            serializer.finish()?;
        }
        QueryResults::Boolean(value) => {
            QueryResultsSerializer::from_format(QueryResultsFormat::Json)
                .serialize_boolean_to_writer(&mut out, value)?;
        }
        QueryResults::Graph(triples) => {
            let mut serializer =
                RdfSerializer::from_format(RdfFormat::NTriples).for_writer(&mut out);
            for triple in triples {
                serializer.serialize_triple(triple?.as_ref())?;
            }
            serializer.finish()?;
        }
    }
    String::from_utf8(out).map_err(|e| anyhow::anyhow!("SPARQL result was not UTF-8: {e}"))
}

/// When a query matched nothing, say whether it could ever have matched.
///
/// An empty SPARQL result is indistinguishable from "this knowledge does not exist",
/// and that reading is usually wrong. The term namespace is not derivable from
/// anything an agent sees: the tool description names GRAPHS, recall prints predicates
/// as bare local names, and instance IRIs live on a different domain from the
/// vocabulary. So an agent writes plausible IRIs from the wrong namespace, gets zero
/// rows, and reports the graph empty — two models did exactly that about 54 accepted
/// Constraints and 74 Lessons (Lesson ea5e5f24).
///
/// This compares the terms the query NAMES against the terms the store USES, and
/// suggests replacements by LOCAL NAME so no namespace is ever hardcoded here
/// (Constraint 19bb4d8a). Returns None when the query names nothing unknown — an
/// empty result is then a real answer, and saying more would be noise.
pub fn explain_empty_result(store: &Store, query: &str, output: &str) -> Option<String> {
    if !is_empty_result(output) {
        return None;
    }
    let named = query_terms(query);
    if named.is_empty() {
        return None;
    }
    let used = store_vocabulary(store).ok()?;
    let unknown: Vec<&String> = named.iter().filter(|t| !used.contains(*t)).collect();
    if unknown.is_empty() {
        return None;
    }
    let mut out = String::from(
        "No rows matched, and these terms in the query do not occur anywhere in this graph:\n",
    );
    for term in unknown.iter().take(6) {
        out.push_str(&format!("  <{term}>\n"));
        for hit in suggest_by_local_name(term, &used).iter().take(3) {
            out.push_str(&format!("      did you mean <{hit}> ?\n"));
        }
    }
    if unknown.len() > 6 {
        out.push_str(&format!("  … and {} more\n", unknown.len() - 6));
    }
    out.push_str(
        "An empty result does NOT mean the graph holds no such knowledge — a query over terms \
         this graph never uses always matches nothing. List the real vocabulary with:\n  \
         SELECT DISTINCT ?p WHERE { ?s ?p ?o }\n  SELECT DISTINCT ?c WHERE { ?s a ?c }\n\
         Then re-run YOUR ORIGINAL query with the corrected terms. This was a vocabulary \
         problem, not a scope problem — widening the query to compensate returns far more \
         than was asked for.",
    );
    Some(out)
}

/// Explain a query that never ran.
///
/// [`explain_empty_result`] only reaches a query that PARSED; one that fails to parse
/// returns early with the raw parser expectation set, which reads as a wall of character
/// classes with the real cause buried inside — Qwen3.5-9B received
/// `expected one of Prefix not found, ['%'], ['-' | '0' ..= '9' | …]` six times in one
/// cell, never recovered, and fell back to grepping the filesystem, despite recall having
/// already handed it the full answer. An undeclared prefix is the most common cause and
/// is cheap to name exactly.
pub fn explain_parse_error(query: &str) -> Option<String> {
    let (_, undeclared) = scan_query(query);
    if undeclared.is_empty() {
        return None;
    }
    let names: Vec<String> = undeclared.iter().map(|p| format!("{p}:")).collect();
    Some(format!(
        "The query uses {} that it never declares: {}. Add a PREFIX line for each, or write \
         the term as a full IRI in angle brackets. This graph's real vocabulary is:\n  \
         SELECT DISTINCT ?p WHERE {{ ?s ?p ?o }}\n  SELECT DISTINCT ?c WHERE {{ ?s a ?c }}",
        if undeclared.len() == 1 {
            "a prefix"
        } else {
            "prefixes"
        },
        names.join(", ")
    ))
}

/// True when a serialized result carries nothing — no solutions, a false ASK, or no
/// triples. All three are the shape an agent misreads as "this knowledge does not exist".
fn is_empty_result(output: &str) -> bool {
    let trimmed = output.trim();
    trimmed.is_empty()
        || trimmed.contains("\"bindings\":[]")
        || trimmed.contains("\"boolean\":false")
}

/// Full IRIs a query names: `<...>` terms plus prefixed names expanded against the
/// query's own PREFIX declarations. Variables, literals and blank nodes are not terms.
fn query_terms(query: &str) -> Vec<String> {
    scan_query(query).0
}

/// Full IRIs a query names, and the prefixes it uses but never declares.
fn scan_query(query: &str) -> (Vec<String>, std::collections::BTreeSet<String>) {
    let mut iris: Vec<String> = Vec::new();
    let mut prefixes: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let bytes: Vec<char> = query.chars().collect();
    let mut i = 0;
    let mut stripped = String::with_capacity(query.len());
    while i < bytes.len() {
        match bytes[i] {
            // An IRI runs to the next '>' and may contain '#', which is also the comment
            // marker — so IRIs must be consumed before comments are stripped.
            '<' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != '>' && !bytes[j].is_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == '>' {
                    let iri: String = bytes[start..j].iter().collect();
                    // `PREFIX name: <iri>` binds rather than names a term.
                    if let Some(name) = trailing_prefix_decl(&stripped) {
                        prefixes.insert(name, iri);
                    } else if iri.contains("://") {
                        iris.push(iri);
                    }
                    stripped.push(' ');
                    i = j + 1;
                    continue;
                }
                i += 1;
            }
            '#' => {
                while i < bytes.len() && bytes[i] != '\n' {
                    i += 1;
                }
            }
            '"' | '\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    i += if bytes[i] == '\\' { 2 } else { 1 };
                }
                i += 1;
                stripped.push(' ');
            }
            c => {
                stripped.push(c);
                i += 1;
            }
        }
    }
    let mut undeclared = std::collections::BTreeSet::new();
    for token in stripped.split(|c: char| c.is_whitespace() || "{}()[],;.".contains(c)) {
        let Some((prefix, local)) = token.split_once(':') else {
            continue;
        };
        if local.is_empty()
            || prefix.is_empty()
            || prefix.starts_with('?')
            || prefix.starts_with('$')
        {
            continue;
        }
        match prefixes.get(prefix) {
            Some(base) => iris.push(format!("{base}{local}")),
            // `rdfs:label` with no PREFIX line is the single most common way a query
            // fails to parse here, and the parser reports it as a wall of character
            // classes with "Prefix not found" buried inside.
            None => {
                undeclared.insert(prefix.to_string());
            }
        }
    }
    iris.sort();
    iris.dedup();
    (iris, undeclared)
}

/// The prefix name when `text` ends with a `PREFIX name:` (or `@prefix name:`) declaration.
fn trailing_prefix_decl(text: &str) -> Option<String> {
    let tail = text.trim_end();
    let name = tail.strip_suffix(':')?;
    let (head, name) = name.rsplit_once(char::is_whitespace).unwrap_or(("", name));
    let keyword = head.trim_end().rsplit(char::is_whitespace).next()?;
    (keyword.eq_ignore_ascii_case("prefix") || keyword.eq_ignore_ascii_case("@prefix"))
        .then(|| name.to_string())
}

/// Every IRI the store actually uses as a predicate or as a class.
fn store_vocabulary(store: &Store) -> anyhow::Result<std::collections::HashSet<String>> {
    let mut terms = std::collections::HashSet::new();
    for q in [
        "SELECT DISTINCT ?t WHERE { ?s ?t ?o }", // predicates
        "SELECT DISTINCT ?t WHERE { ?s a ?t }",  // classes
        // Subjects and IRI objects, or every RECORD iri a query names is reported as absent.
        // Record IRIs are subjects, so scanning only predicates and classes made the claim
        // "does not occur anywhere in this graph" structurally guaranteed to be wrong about
        // them: a query for a record that exists, returning empty for any other reason, was
        // told the record does not exist. That is this diagnostic causing the failure it was
        // written to prevent (Lesson ea5e5f24 — models concluding a graph of 74 Lessons held
        // none). Four scans instead of two, and only on an already-empty result.
        "SELECT DISTINCT ?t WHERE { ?t ?p ?o }",
        "SELECT DISTINCT ?t WHERE { ?s ?p ?t }",
    ] {
        let mut prepared = SparqlEvaluator::new().parse_query(q)?;
        if prepared.dataset().is_default_dataset() {
            prepared.dataset_mut().set_default_graph_as_union();
        }
        if let QueryResults::Solutions(solutions) = prepared.on_store(store).execute()? {
            for solution in solutions {
                if let Some(oxigraph::model::Term::NamedNode(n)) = solution?.get("t") {
                    terms.insert(n.as_str().to_string());
                }
            }
        }
    }
    Ok(terms)
}

/// Terms whose local name matches the unknown term's, exactly or by containment —
/// `status` finds `hasLifecycleStatus`, which is the miss that actually happens.
fn suggest_by_local_name(unknown: &str, used: &std::collections::HashSet<String>) -> Vec<String> {
    let local =
        |iri: &str| -> String { iri.rsplit(['#', '/']).next().unwrap_or(iri).to_lowercase() };
    let needle = local(unknown);
    if needle.is_empty() {
        return Vec::new();
    }
    let mut exact = Vec::new();
    let mut partial = Vec::new();
    for candidate in used {
        let hay = local(candidate);
        if hay == needle {
            exact.push(candidate.clone());
        } else if hay.contains(&needle) {
            partial.push(candidate.clone());
        }
    }
    exact.sort();
    partial.sort();
    exact.extend(partial);
    exact
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxigraph::model::{GraphNameRef, NamedNodeRef, QuadRef};

    const ARCH: &str = "https://trivyn.io/ontologies/software/architecture#";
    /// The graph IRI the sparql tool description advertises — and the string the 122B
    /// turned into a term namespace by appending '#'.
    const SHAPES_GRAPH: &str = "https://moosedev.dev/kg/ontology/software-architecture/shapes#";

    fn store() -> Store {
        let store = Store::new().unwrap();
        let subject = NamedNodeRef::new("https://moosedev.dev/kg/Constraint/c1").unwrap();
        for (p, o) in [
            (
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                format!("{ARCH}Constraint"),
            ),
            (&format!("{ARCH}hasLifecycleStatus"), "accepted".into()),
        ] {
            store
                .insert(QuadRef::new(
                    subject,
                    NamedNodeRef::new(p).unwrap(),
                    NamedNodeRef::new(&o).unwrap_or(subject),
                    GraphNameRef::DefaultGraph,
                ))
                .unwrap();
        }
        store
    }

    /// A record IRI that EXISTS must never be reported as absent. `store_vocabulary` once
    /// scanned only predicates and classes; records are SUBJECTS, so every record a query
    /// named was declared "not in this graph", and a query returning empty for any other
    /// reason blamed the record instead of the real cause. That is this diagnostic causing
    /// the misreading it was written to prevent (Lesson ea5e5f24).
    #[test]
    fn an_existing_record_is_not_reported_as_unknown() {
        let empty = r#"{"results":{"bindings":[]}}"#;
        let rec = "https://moosedev.dev/kg/Constraint/c1"; // the subject store() inserts

        // Empty because the PREDICATE is absent, not the record.
        let q = format!("SELECT ?d WHERE {{ <{rec}> <{ARCH}hasDescription> ?d }}");
        let note = explain_empty_result(&store(), &q, empty).expect("a note");
        assert!(
            !note.contains(rec),
            "the record EXISTS and must not be named:\n{note}"
        );
        assert!(
            note.contains("hasDescription"),
            "name the absent predicate:\n{note}"
        );

        // A record that genuinely is absent must still be named.
        let gone = "https://moosedev.dev/kg/Constraint/nope";
        let q2 = format!("SELECT ?t WHERE {{ <{gone}> <{ARCH}hasLifecycleStatus> ?t }}");
        let note2 = explain_empty_result(&store(), &q2, empty).expect("a note");
        assert!(
            note2.contains(gone),
            "an absent record must be named:\n{note2}"
        );
    }

    /// The exact failure from the floor study: the tool's own shapes-GRAPH IRI used as a
    /// term namespace. The query is valid, matches nothing, and must not be left to read
    /// as "there are no Constraints".
    #[test]
    fn names_the_terms_the_graph_never_uses() {
        let query = format!(
            "PREFIX kg: <{SHAPES_GRAPH}>\nSELECT ?t WHERE {{ ?c a kg:Constraint . \
             ?c kg:status \"accepted\" }}"
        );
        let note = explain_empty_result(&store(), &query, r#"{"results":{"bindings":[]}}"#)
            .expect("an empty result over unknown terms must be explained");
        assert!(
            note.contains(&format!("{SHAPES_GRAPH}Constraint")),
            "{note}"
        );
        assert!(note.contains(&format!("{SHAPES_GRAPH}status")), "{note}");
        // Local-name matching carries the agent to the real namespace without this code
        // ever naming it (Constraint 19bb4d8a).
        assert!(note.contains(&format!("{ARCH}Constraint")), "{note}");
        assert!(
            note.contains(&format!("{ARCH}hasLifecycleStatus")),
            "{note}"
        );
    }

    /// A correct query that simply matches nothing is a real answer; adding a lecture to it
    /// would train agents to ignore the note that matters.
    #[test]
    fn stays_quiet_when_every_term_is_real() {
        let query = format!(
            "SELECT ?c WHERE {{ ?c a <{ARCH}Constraint> ; \
             <{ARCH}hasLifecycleStatus> \"superseded\" }}"
        );
        assert!(explain_empty_result(&store(), &query, r#"{"results":{"bindings":[]}}"#).is_none());
    }

    /// A non-empty result is never annotated, whatever the query named.
    #[test]
    fn stays_quiet_when_rows_came_back() {
        let query = format!("SELECT ?c WHERE {{ ?c a <{SHAPES_GRAPH}Constraint> }}");
        let rows = r#"{"results":{"bindings":[{"c":{"type":"uri","value":"x"}}]}}"#;
        assert!(explain_empty_result(&store(), &query, rows).is_none());
    }

    /// A PREFIX declaration binds a namespace; it does not name a term to be flagged.
    #[test]
    fn prefix_declarations_are_not_terms() {
        let terms = query_terms(&format!(
            "PREFIX a: <{ARCH}>\nSELECT ?s WHERE {{ ?s a a:Lesson }}"
        ));
        assert_eq!(terms, vec![format!("{ARCH}Lesson")]);
    }

    /// '#' opens a comment in SPARQL and also separates a hash namespace, so IRIs have to be
    /// consumed before comments are stripped or every hash IRI loses its local name.
    #[test]
    fn comments_do_not_truncate_hash_iris() {
        let terms = query_terms(&format!(
            "# find the lessons\nSELECT ?s WHERE {{ ?s a <{ARCH}Lesson> }} # trailing"
        ));
        assert_eq!(terms, vec![format!("{ARCH}Lesson")]);
    }

    /// The exact query Qwen3.5-9B sent six times, whose only fault is the missing
    /// `rdfs:` declaration — the parser reported it as a character-class wall.
    #[test]
    fn names_the_prefix_a_query_forgot_to_declare() {
        let note = explain_parse_error(
            "PREFIX arch: <https://example.org/a#>\nSELECT ?label WHERE { ?r a arch:Requirement . \
             ?r rdfs:label ?label }",
        )
        .expect("an undeclared prefix must be named");
        assert!(note.contains("rdfs:"), "{note}");
        assert!(
            !note.contains("arch:"),
            "a DECLARED prefix is not a fault: {note}"
        );
    }

    /// A syntactically broken query with every prefix declared gets no prefix advice —
    /// inventing a cause would be worse than the parser's own message.
    #[test]
    fn stays_quiet_when_every_prefix_is_declared() {
        assert!(explain_parse_error("SELECT ?s WHERE { ?s ?p ?o } WHERE {").is_none());
        assert!(
            explain_parse_error("SELECT ?id WHERE { ?r <https://example.org/a#x> ?id }").is_none()
        );
    }

    /// The empty-result note must steer back to the original query, not to breadth:
    /// the 27B learned the vocabulary from it, then queried broadly and over-retrieved
    /// (set_superseded_ads precision 0.982 -> 0.443 at unchanged recall).
    #[test]
    fn the_empty_result_note_steers_back_to_the_original_query() {
        let query = format!("SELECT ?t WHERE {{ ?c a <{SHAPES_GRAPH}Constraint> }}");
        let note = explain_empty_result(&store(), &query, r#"{"results":{"bindings":[]}}"#)
            .expect("unknown terms are explained");
        assert!(note.contains("re-run YOUR ORIGINAL query"), "{note}");
        assert!(note.contains("not a scope problem"), "{note}");
    }

    /// An ASK that comes back false is the same trap wearing a different shape.
    #[test]
    fn a_false_ask_is_an_empty_result() {
        assert!(is_empty_result(r#"{"head":{},"boolean":false}"#));
        assert!(!is_empty_result(r#"{"head":{},"boolean":true}"#));
        assert!(is_empty_result("   "));
    }
}
