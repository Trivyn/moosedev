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
