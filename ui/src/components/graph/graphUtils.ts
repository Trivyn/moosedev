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
const MOOSE_NS = 'https://trivyn.io/ontologies/moose#';
const INCOMING_PREDICATE = 'urn:moosedev:incomingPredicate';
const EDGE_PREDICATE = 'urn:moosedev:predicate';
const MOOSE_TRACE_SUPPORT_LOCALS = new Set([
  'QueryExecution',
  'StageRun',
  'Pipeline',
  'Stage',
  'WalkStrategy',
  'LLMSensorPoint',
  'SchemaIntentKind',
  'MOOSE-Pipeline',
  'executes',
  'usedStage',
  'stageInstanceOf',
  'durationMs',
  'usedWalkStrategy',
  'llmSensorInvocations',
  'stageDetail',
  'usedSchemaKind',
]);

interface GraphTriple {
  subject: QueryValue;
  predicate: QueryValue;
  object: QueryValue;
}

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

function isResourceValue(value: QueryValue): boolean {
  return value.type === 'uri' || value.type === 'bnode';
}

function localName(value: string): string {
  const parts = value.split(/[\/#]/).filter(Boolean);
  return parts[parts.length - 1] || value;
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

  const hidden = traceHiddenNodeIds(graph);
  const nodes = graph.nodes.filter((node) => !hidden.has(node.id));
  const visibleNodeIds = new Set(nodes.map((node) => node.id));
  const edges = graph.edges.filter((edge) => visibleNodeIds.has(edge.source) && visibleNodeIds.has(edge.target));
  return { nodes, edges };
}

function traceHiddenNodeIds(graph: GraphData): Set<string> {
  const hidden = new Set<string>();
  const worklist: string[] = [];

  const hide = (id: string) => {
    if (hidden.has(id)) return;
    hidden.add(id);
    worklist.push(id);
  };

  graph.nodes.forEach((node) => {
    if (node.type === 'mooseTrace' || isMooseTraceSupportTerm(node.id)) hide(node.id);
  });

  for (let index = 0; index < worklist.length; index += 1) {
    const id = worklist[index];
    graph.edges.forEach((edge) => {
      if (edge.source === id) hideMooseEndpoint(edge.target, hide);
      if (edge.target === id) hideMooseEndpoint(edge.source, hide);
    });
  }

  return hidden;
}

function hideMooseEndpoint(id: string, hide: (id: string) => void) {
  if (id.startsWith(MOOSE_NS)) hide(id);
}

function isMooseTraceSupportTerm(value: string): boolean {
  if (!value.startsWith(MOOSE_NS)) return false;
  const local = localName(value);
  return (
    MOOSE_TRACE_SUPPORT_LOCALS.has(local) ||
    local.startsWith('Stage') ||
    local.startsWith('WalkStrategy') ||
    local.startsWith('LLMSensor-')
  );
}

function resultTriples(result: QueryResponse): GraphTriple[] {
  const triples: GraphTriple[] = [...(result.triples ?? [])];
  const bindingTriple = (binding: Record<string, QueryValue>) => ({
    subject: binding.subject ?? binding.s,
    predicate: binding.predicate ?? binding.p,
    object: binding.object ?? binding.o,
  });

  result.results?.bindings.forEach((binding) => {
    const { subject, predicate, object } = bindingTriple(binding);
    if (subject && predicate && object) triples.push({ subject, predicate, object });
  });

  return triples;
}

function labelMap(triples: GraphTriple[]): Map<string, string> {
  const labels = new Map<string, string>();
  triples.forEach(({ subject, predicate, object }) => {
    if (isResourceValue(subject) && predicate.value === RDFS_LABEL && object.type === 'literal') {
      labels.set(subject.value, object.value);
    }
  });
  return labels;
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
  const triples = resultTriples(result);
  const labels = labelMap(triples);
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
    if (!isResourceValue(term)) return;
    nodeTerms.set(term.value, term);
    const properties = propertiesFor(term.value);
    const labelValue = properties.get(RDFS_LABEL)?.find((value) => value.type === 'literal')?.value;
    const rdfTypes = properties.get(RDF_TYPE) ?? [];
    const existing = nodes.get(term.value);
    const next = {
      id: term.value,
      label: labelValue ?? labels.get(term.value) ?? existing?.label ?? shortName(term.value),
      type: graphNodeType(term, rdfTypes),
      properties: mapProperties(properties),
    };
    nodes.set(term.value, next);
  };

  const attachProperty = (subject: QueryValue, predicate: QueryValue, object: QueryValue) => {
    if (!isResourceValue(subject)) return;
    setProperty(propertiesFor(subject.value), predicate.value, object);
    const subjectTerm = nodeTerms.get(subject.value) ?? subject;
    addNode(subjectTerm);
  };

  const addTriple = (subject: QueryValue, predicate: QueryValue, object: QueryValue, index: number) => {
    attachProperty(subject, predicate, object);
    if (isResourceValue(object)) {
      attachProperty(object, { type: 'uri', value: INCOMING_PREDICATE }, predicate);
      addEdge(subject, predicate, object, index);
    }
  };

  const addEdge = (subject: QueryValue, predicate: QueryValue, object: QueryValue, index: number) => {
    if (!isResourceValue(subject) || !isResourceValue(object)) {
      return;
    }
    addNode(subject);
    addNode(object);
    const id = `${subject.value}|${predicate.value}|${object.value}|${index}`;
    edges.set(id, {
      id,
      source: subject.value,
      target: object.value,
      label: labels.get(predicate.value) ?? shortName(predicate.value),
      type: shortName(predicate.value),
      predicate: predicate.value,
      properties: [{ predicate: EDGE_PREDICATE, values: [predicate] }],
    });
  };

  triples.forEach((triple, index) => addTriple(triple.subject, triple.predicate, triple.object, index));

  return filterGraph({ nodes: [...nodes.values()], edges: [...edges.values()] }, options);
}
