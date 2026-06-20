use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use oxigraph::model::{GraphName, NamedNode, NamedOrBlankNodeRef, Term, TermRef};
use oxigraph::sparql::{QueryResults, SparqlEvaluator};

use crate::api::error::ApiError;
use crate::api::models::{
    QueryBinding, QueryHead, QueryResponse, QueryResults as ApiQueryResults, QueryValue,
    SparqlQueryRequest, TriplePayload,
};
use crate::graph::{AppState, PROJECT_KG_GRAPH_IRI};

/// Execute read-only SPARQL for the human UI.
///
/// Unlike the MCP `sparql` tool, the UI defaults to the project KG only. That
/// prevents ontology/provenance triples from flooding simple exploratory views
/// while still letting advanced users opt into other graphs with explicit FROM.
pub async fn query(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<SparqlQueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let query = payload.query.trim();
    if query.is_empty() {
        return Err(ApiError::bad_request("query must not be empty"));
    }
    run_project_query(&state, query).map(Json)
}

fn run_project_query(state: &AppState, query: &str) -> Result<QueryResponse, ApiError> {
    let mut prepared = SparqlEvaluator::new()
        .parse_query(query)
        .map_err(|e| ApiError::bad_request(format!("parse query: {e}")))?;
    if prepared.dataset().is_default_dataset() {
        // Oxigraph lets us set the dataset after parsing. This preserves the
        // user's query text while making plain `WHERE { ?s ?p ?o }` mean "the
        // project graph", not the union of every named graph in the store.
        let graph = NamedNode::new(PROJECT_KG_GRAPH_IRI)
            .map_err(|e| ApiError::internal(format!("invalid project graph IRI: {e}")))?;
        prepared
            .dataset_mut()
            .set_default_graph(vec![GraphName::NamedNode(graph)]);
    }
    let results = prepared
        .on_store(&state.store)
        .execute()
        .map_err(|e| ApiError::bad_request(format!("execute query: {e}")))?;
    convert_results(results)
}

fn convert_results(results: QueryResults<'_>) -> Result<QueryResponse, ApiError> {
    match results {
        QueryResults::Solutions(solutions) => {
            let vars: Vec<String> = solutions
                .variables()
                .iter()
                .map(|v| v.as_str().to_string())
                .collect();
            let mut bindings = Vec::new();
            for solution in solutions {
                let solution =
                    solution.map_err(|e| ApiError::internal(format!("read solution: {e}")))?;
                let mut row = HashMap::new();
                for var in &vars {
                    if let Some(value) = solution.get(var.as_str()) {
                        row.insert(var.clone(), term_value(value));
                    }
                }
                bindings.push(QueryBinding { bindings: row });
            }
            Ok(QueryResponse {
                query_type: "SELECT".to_string(),
                head: Some(QueryHead { vars }),
                results: Some(ApiQueryResults { bindings }),
                boolean: None,
                triples: None,
            })
        }
        QueryResults::Boolean(value) => Ok(QueryResponse {
            query_type: "ASK".to_string(),
            head: None,
            results: None,
            boolean: Some(value),
            triples: None,
        }),
        QueryResults::Graph(triples) => {
            let mut out = Vec::new();
            for triple in triples {
                let triple =
                    triple.map_err(|e| ApiError::internal(format!("read graph result: {e}")))?;
                out.push(TriplePayload {
                    subject: named_or_blank_value(triple.subject.as_ref()),
                    predicate: QueryValue::uri(triple.predicate.as_str()),
                    object: term_ref_value(triple.object.as_ref()),
                });
            }
            Ok(QueryResponse {
                // DESCRIBE also arrives as `QueryResults::Graph`; the UI only
                // needs the graph-result contract, so CONSTRUCT is the stable
                // display family name here.
                query_type: "CONSTRUCT".to_string(),
                head: None,
                results: None,
                boolean: None,
                triples: Some(out),
            })
        }
    }
}

fn named_or_blank_value(value: NamedOrBlankNodeRef<'_>) -> QueryValue {
    match value {
        oxigraph::model::NamedOrBlankNodeRef::NamedNode(node) => QueryValue::uri(node.as_str()),
        oxigraph::model::NamedOrBlankNodeRef::BlankNode(node) => QueryValue::bnode(node.as_str()),
    }
}

fn term_value(value: &Term) -> QueryValue {
    match value {
        Term::NamedNode(node) => QueryValue::uri(node.as_str()),
        Term::BlankNode(node) => QueryValue::bnode(node.as_str()),
        Term::Literal(lit) => QueryValue::literal(
            lit.value(),
            lit.datatype().as_str(),
            lit.language().map(str::to_string),
        ),
        #[allow(unreachable_patterns)]
        _ => QueryValue::unknown(value.to_string()),
    }
}

fn term_ref_value(value: TermRef<'_>) -> QueryValue {
    match value {
        TermRef::NamedNode(node) => QueryValue::uri(node.as_str()),
        TermRef::BlankNode(node) => QueryValue::bnode(node.as_str()),
        TermRef::Literal(lit) => QueryValue::literal(
            lit.value(),
            lit.datatype().as_str(),
            lit.language().map(str::to_string),
        ),
        #[allow(unreachable_patterns)]
        _ => QueryValue::unknown(value.to_string()),
    }
}
