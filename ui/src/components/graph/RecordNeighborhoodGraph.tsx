import { useEffect, useMemo, useState } from 'react';
import { Alert, Box, CircularProgress } from '@mui/material';
import { api } from '../../api/client';
import { GraphEdge, GraphNode, RecordDetailResponse } from '../../api/types';
import CytoscapeGraph from './CytoscapeGraph';

export interface RecordNeighborhoodData {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

interface RecordNeighborhoodGraphCommonProps {
  onNavigateRecord?: (iri: string) => void;
  height?: number | string;
}

type RecordNeighborhoodGraphProps = RecordNeighborhoodGraphCommonProps &
  (
    | { uuid: string; record?: never }
    | { uuid?: never; record: RecordDetailResponse }
  );

function edgeId(source: string, predicate: string, target: string): string {
  return `record-edge:${encodeURIComponent(source)}:${encodeURIComponent(predicate)}:${encodeURIComponent(target)}`;
}

function isProjectRecordIri(iri: string): boolean {
  try {
    const url = new URL(iri);
    return url.origin === 'https://moosedev.dev' && /^\/kg\/[^/]+\/[^/]+$/.test(url.pathname);
  } catch {
    return false;
  }
}

/** Convert a record-detail response into its directed, one-hop neighborhood. */
export function recordToNeighborhoodGraph(record: RecordDetailResponse): RecordNeighborhoodData {
  const nodes = new Map<string, GraphNode>();
  const edges = new Map<string, GraphEdge>();

  const addNode = (id: string, label: string, kind: string) => {
    if (nodes.has(id)) return;
    nodes.set(id, {
      id,
      label: label || id,
      type: isProjectRecordIri(id) ? 'projectRecord' : 'uri',
      properties: kind
        ? [{ predicate: 'urn:moosedev:recordKind', values: [{ type: 'literal', value: kind }] }]
        : [],
    });
  };

  const addEdge = (source: string, predicate: string, target: string) => {
    const id = edgeId(source, predicate, target);
    if (edges.has(id)) return;
    edges.set(id, {
      id,
      source,
      target,
      label: predicate,
      type: predicate,
      predicate,
    });
  };

  addNode(record.iri, record.title, record.kind);

  record.outgoing.forEach((edge) => {
    addNode(edge.target_iri, edge.target_label, edge.target_kind);
    addEdge(record.iri, edge.predicate, edge.target_iri);
  });
  record.incoming.forEach((edge) => {
    addNode(edge.source_iri, edge.source_label, edge.source_kind);
    addEdge(edge.source_iri, edge.predicate, record.iri);
  });

  return { nodes: [...nodes.values()], edges: [...edges.values()] };
}

export default function RecordNeighborhoodGraph(props: RecordNeighborhoodGraphProps) {
  const { onNavigateRecord, height = 360 } = props;
  const providedRecord = props.record;
  const uuid = props.uuid;
  const [loadedRecord, setLoadedRecord] = useState<RecordDetailResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (providedRecord) {
      setLoadedRecord(null);
      setError(null);
      return;
    }

    let cancelled = false;
    setLoadedRecord(null);
    setError(null);
    api
      .record(uuid!)
      .then((response) => {
        if (!cancelled) setLoadedRecord(response);
      })
      .catch((reason) => {
        if (!cancelled) setError(reason instanceof Error ? reason.message : String(reason));
      });
    return () => {
      cancelled = true;
    };
  }, [providedRecord, uuid]);

  const record = providedRecord ?? loadedRecord;
  const graph = useMemo(() => (record ? recordToNeighborhoodGraph(record) : null), [record]);

  return (
    <Box sx={{ height, minHeight: 240, position: 'relative' }} aria-label="Record relationships">
      {error ? (
        <Alert severity="warning">Could not load relationships: {error}</Alert>
      ) : !graph ? (
        <Box sx={{ height: '100%', display: 'grid', placeItems: 'center' }}>
          <CircularProgress size={22} aria-label="Loading record relationships" />
        </Box>
      ) : (
        <CytoscapeGraph
          nodes={graph.nodes}
          edges={graph.edges}
          mode="navigate"
          focusNodeId={record?.iri}
          onNodeClick={(node) => {
            if (node.id !== record?.iri && isProjectRecordIri(node.id)) {
              onNavigateRecord?.(node.id);
            }
          }}
        />
      )}
    </Box>
  );
}
