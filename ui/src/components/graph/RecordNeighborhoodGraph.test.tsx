// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RecordDetailResponse } from '../../api/types';
import RecordNeighborhoodGraph, { recordToNeighborhoodGraph } from './RecordNeighborhoodGraph';

vi.mock('./CytoscapeGraph', () => ({
  default: ({ nodes, focusNodeId, onNodeClick }: {
    nodes: Array<{ id: string; label: string }>;
    focusNodeId?: string;
    onNodeClick?: (node: { id: string; label: string }) => void;
  }) => (
    <div data-testid="cytoscape" data-focus={focusNodeId}>
      {nodes.map((node) => (
        <button key={node.id} onClick={() => onNodeClick?.(node)}>{node.label}</button>
      ))}
    </div>
  ),
}));

const center = 'https://moosedev.dev/kg/Constraint/center';
const neighbor = 'https://moosedev.dev/kg/Requirement/neighbor';

function record(overrides: Partial<RecordDetailResponse> = {}): RecordDetailResponse {
  return {
    iri: center,
    kind: 'Constraint',
    title: 'Center',
    description: null,
    status: 'accepted',
    timestamp: null,
    author: null,
    outgoing: [],
    incoming: [],
    ...overrides,
  };
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('recordToNeighborhoodGraph', () => {
  it('preserves incoming and outgoing direction while deduplicating shared nodes and exact triples', () => {
    const graph = recordToNeighborhoodGraph(record({
      outgoing: [
        { predicate: 'constrains', target_iri: neighbor, target_label: 'Neighbor', target_kind: 'Requirement' },
        { predicate: 'constrains', target_iri: neighbor, target_label: 'Neighbor', target_kind: 'Requirement' },
        { predicate: 'concerns', target_iri: neighbor, target_label: 'Neighbor', target_kind: 'Requirement' },
      ],
      incoming: [
        { predicate: 'dependsOn', source_iri: neighbor, source_label: 'Neighbor', source_kind: 'Requirement' },
      ],
    }));

    expect(graph.nodes.map((node) => node.id)).toEqual([center, neighbor]);
    expect(graph.edges.map(({ source, label, target }) => ({ source, label, target }))).toEqual([
      { source: center, label: 'constrains', target: neighbor },
      { source: center, label: 'concerns', target: neighbor },
      { source: neighbor, label: 'dependsOn', target: center },
    ]);
  });

  it('keeps a self-loop and gives exact triples stable IDs independent of response order', () => {
    const first = recordToNeighborhoodGraph(record({
      outgoing: [
        { predicate: 'relatedTo', target_iri: center, target_label: 'Center', target_kind: 'Constraint' },
        { predicate: 'constrains', target_iri: neighbor, target_label: 'Neighbor', target_kind: 'Requirement' },
      ],
    }));
    const second = recordToNeighborhoodGraph(record({ outgoing: [...record({
      outgoing: [
        { predicate: 'relatedTo', target_iri: center, target_label: 'Center', target_kind: 'Constraint' },
        { predicate: 'constrains', target_iri: neighbor, target_label: 'Neighbor', target_kind: 'Requirement' },
      ],
    }).outgoing].reverse() }));

    expect(first.edges.some((edge) => edge.source === center && edge.target === center)).toBe(true);
    expect(first.edges.map((edge) => edge.id).sort()).toEqual(second.edges.map((edge) => edge.id).sort());
  });
});

describe('RecordNeighborhoodGraph', () => {
  it('loads the one-hop record graph and navigates only non-focus project records', async () => {
    const onNavigateRecord = vi.fn();
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue({
      ok: true,
      json: async () => record({
        outgoing: [
          { predicate: 'constrains', target_iri: neighbor, target_label: 'Neighbor', target_kind: 'Requirement' },
          { predicate: 'documents', target_iri: 'https://example.test/docs', target_label: 'External', target_kind: '' },
        ],
      }),
    }));

    render(<RecordNeighborhoodGraph uuid="center" onNavigateRecord={onNavigateRecord} />);

    const graph = await screen.findByTestId('cytoscape');
    expect(graph).toHaveAttribute('data-focus', center);
    screen.getByRole('button', { name: 'Center' }).click();
    screen.getByRole('button', { name: 'External' }).click();
    screen.getByRole('button', { name: 'Neighbor' }).click();
    expect(onNavigateRecord).toHaveBeenCalledTimes(1);
    expect(onNavigateRecord).toHaveBeenCalledWith(neighbor);
  });

  it('renders the center node for a record with no relationships without fetching again', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);

    render(<RecordNeighborhoodGraph record={record()} />);

    const graph = await screen.findByTestId('cytoscape');
    expect(graph).toHaveAttribute('data-focus', center);
    expect(screen.getByRole('button', { name: 'Center' })).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('suppresses stale responses and keeps relationship errors local', async () => {
    let resolveFirst!: (value: unknown) => void;
    const first = new Promise((resolve) => { resolveFirst = resolve; });
    const fetchMock = vi.fn()
      .mockReturnValueOnce(first)
      .mockResolvedValueOnce({ ok: true, json: async () => record({ title: 'Current' }) });
    vi.stubGlobal('fetch', fetchMock);
    const { rerender } = render(<RecordNeighborhoodGraph uuid="old" />);
    rerender(<RecordNeighborhoodGraph uuid="new" />);

    expect(await screen.findByTestId('cytoscape')).toBeInTheDocument();
    resolveFirst({ ok: true, json: async () => record({ title: 'Stale' }) });
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    expect(screen.getByTestId('cytoscape')).toBeInTheDocument();

    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    rerender(<RecordNeighborhoodGraph uuid="error" />);
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load relationships: offline');
  });
});
