import { GraphEdge, GraphNode, GraphProperty, QueryResponse, QueryValue } from '../../api/types';

export interface GraphData {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface GraphOptions {
  showMooseTraces?: boolean;
}

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

const RDF_TYPE = 'http://www.w3.org/1999/02/22-rdf-syntax-ns#type';
const RDFS_LABEL = 'http://www.w3.org/2000/01/rdf-schema#label';

export function shortName(value: string): string {
  for (const [prefix, replacement] of Object.entries(PREFIXES)) {
    if (value.startsWith(prefix)) return value.replace(prefix, replacement);
  }
  const parts = value.split(/[\/#]/).filter(Boolean);
  return parts[parts.length - 1] || value;
}

function graphNodeType(term: QueryValue, rdfTypes: QueryValue[] = []): string {
  if (term.type === 'bnode') return 'bnode';

  const values = [term.value, ...rdfTypes.map((value) => value.value)].map((value) => value.toLowerCase());
  if (values.some((value) => value.includes('/execution/') || value.includes('/stage-run/') || value.includes('executiontrace') || value.includes('stagerun'))) {
    return 'mooseTrace';
  }
  if (term.value.startsWith('https://moosedev.dev/kg/')) return 'projectRecord';
  if (term.value.includes('www.w3.org/')) return 'schema';
  if (term.value.includes('trivyn.io/ontologies/software/')) return 'ontology';
  return term.type;
}

function setProperty(properties: Map<string, QueryValue[]>, predicate: string, value: QueryValue) {
  const values = properties.get(predicate) ?? [];
  values.push(value);
  properties.set(predicate, values);
}

function mapProperties(properties: Map<string, QueryValue[]>): GraphProperty[] {
  return [...properties.entries()]
    .sort(([left], [right]) => shortName(left).localeCompare(shortName(right)))
    .map(([predicate, values]) => ({ predicate, values }));
}

function filterGraph(graph: GraphData, options: GraphOptions = {}): GraphData {
  if (options.showMooseTraces !== false) return graph;

  const nodes = graph.nodes.filter((node) => node.type !== 'mooseTrace');
  const visibleNodeIds = new Set(nodes.map((node) => node.id));
  const edges = graph.edges.filter((edge) => visibleNodeIds.has(edge.source) && visibleNodeIds.has(edge.target));
  return { nodes, edges };
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
export function queryToGraph(result?: QueryResponse | null, options: GraphOptions = {}): GraphData {
  if (!result) return { nodes: [], edges: [] };
  const nodes = new Map<string, GraphNode>();
  const edges = new Map<string, GraphEdge>();
  const nodeProperties = new Map<string, Map<string, QueryValue[]>>();
  const nodeTerms = new Map<string, QueryValue>();

  const propertiesFor = (id: string) => {
    let properties = nodeProperties.get(id);
    if (!properties) {
      properties = new Map();
      nodeProperties.set(id, properties);
    }
    return properties;
  };

  const addNode = (term: QueryValue) => {
    if (term.type !== 'uri' && term.type !== 'bnode') return;
    nodeTerms.set(term.value, term);
    const properties = propertiesFor(term.value);
    const labelValue = properties.get(RDFS_LABEL)?.find((value) => value.type === 'literal')?.value;
    const rdfTypes = properties.get(RDF_TYPE) ?? [];
    const existing = nodes.get(term.value);
    const next = {
      id: term.value,
      label: labelValue ?? existing?.label ?? shortName(term.value),
      type: graphNodeType(term, rdfTypes),
      properties: mapProperties(properties),
    };
    nodes.set(term.value, next);
  };

  const attachProperty = (subject: QueryValue, predicate: QueryValue, object: QueryValue) => {
    if (subject.type !== 'uri' && subject.type !== 'bnode') return;
    setProperty(propertiesFor(subject.value), predicate.value, object);
    const subjectTerm = nodeTerms.get(subject.value) ?? subject;
    addNode(subjectTerm);
  };

  const addTriple = (subject: QueryValue, predicate: QueryValue, object: QueryValue, index: number) => {
    attachProperty(subject, predicate, object);
    if (object.type === 'uri' || object.type === 'bnode') {
      attachProperty(object, { type: 'uri', value: 'urn:moosedev:incomingPredicate' }, predicate);
      addEdge(subject, predicate, object, index);
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
      predicate: predicate.value,
      properties: [{ predicate: 'urn:moosedev:predicate', values: [predicate] }],
    });
  };

  if (result.triples) {
    result.triples.forEach((triple, index) => addTriple(triple.subject, triple.predicate, triple.object, index));
  }

  const bindingTriple = (binding: Record<string, QueryValue>) => ({
    subject: binding.subject ?? binding.s,
    predicate: binding.predicate ?? binding.p,
    object: binding.object ?? binding.o,
  });

  result.results?.bindings.forEach((binding, index) => {
    const { subject, predicate, object } = bindingTriple(binding);
    if (subject && predicate && object) addTriple(subject, predicate, object, index);
  });

  return filterGraph({ nodes: [...nodes.values()], edges: [...edges.values()] }, options);
}
