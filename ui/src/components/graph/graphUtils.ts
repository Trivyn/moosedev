import { GraphEdge, GraphNode, QueryResponse, QueryValue } from '../../api/types';

const PREFIXES: Record<string, string> = {
  'http://www.w3.org/1999/02/22-rdf-syntax-ns#': 'rdf:',
  'http://www.w3.org/2000/01/rdf-schema#': 'rdfs:',
  'http://www.w3.org/2002/07/owl#': 'owl:',
  'http://www.w3.org/ns/prov#': 'prov:',
  'http://www.w3.org/ns/shacl#': 'sh:',
  'https://moosedev.dev/kg/': 'kg:',
  'https://trivyn.io/ontologies/software/architecture/domain/': 'arch:',
  'https://trivyn.io/ontologies/software/engineering/domain/': 'se:',
};

export function shortName(value: string): string {
  for (const [prefix, replacement] of Object.entries(PREFIXES)) {
    if (value.startsWith(prefix)) return value.replace(prefix, replacement);
  }
  const parts = value.split(/[\/#]/).filter(Boolean);
  return parts[parts.length - 1] || value;
}

/**
 * Convert SPARQL-ish API results into a Cytoscape graph.
 *
 * The backend returns SELECT rows and graph results in different JSON shapes.
 * For visualization, both are normalized to triples when they expose the
 * conventional `subject/predicate/object` variables. Literal objects are kept
 * in table/raw views and deliberately omitted here so the graph is not swamped
 * by value nodes.
 */
export function queryToGraph(result?: QueryResponse | null): { nodes: GraphNode[]; edges: GraphEdge[] } {
  if (!result) return { nodes: [], edges: [] };
  const nodes = new Map<string, GraphNode>();
  const edges = new Map<string, GraphEdge>();

  const addNode = (term: QueryValue) => {
    if (term.type !== 'uri' && term.type !== 'bnode') return;
    if (!nodes.has(term.value)) {
      nodes.set(term.value, {
        id: term.value,
        label: shortName(term.value),
        type: term.type,
      });
    }
  };

  const addEdge = (subject: QueryValue, predicate: QueryValue, object: QueryValue, index: number) => {
    if ((subject.type !== 'uri' && subject.type !== 'bnode') || (object.type !== 'uri' && object.type !== 'bnode')) {
      return;
    }
    addNode(subject);
    addNode(object);
    const id = `${subject.value}|${predicate.value}|${object.value}|${index}`;
    edges.set(id, {
      id,
      source: subject.value,
      target: object.value,
      label: shortName(predicate.value),
      type: shortName(predicate.value),
    });
  };

  if (result.triples) {
    result.triples.forEach((triple, index) => addEdge(triple.subject, triple.predicate, triple.object, index));
  }

  const bindingTriple = (binding: Record<string, QueryValue>) => ({
    subject: binding.subject ?? binding.s,
    predicate: binding.predicate ?? binding.p,
    object: binding.object ?? binding.o,
  });

  result.results?.bindings.forEach((binding, index) => {
    const { subject, predicate, object } = bindingTriple(binding);
    if (subject && predicate && object) addEdge(subject, predicate, object, index);
  });

  return { nodes: [...nodes.values()], edges: [...edges.values()] };
}
