// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import RecordPage from './RecordPage';

vi.mock('../components/graph/RecordNeighborhoodGraph', () => ({
  default: ({ record }: { record: typeof response }) => (
    <div>Relationship graph for {record.title}</div>
  ),
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const response = {
  iri: 'https://moosedev.dev/kg/Constraint/record-1',
  kind: 'Constraint',
  title: 'Keep local operation',
  description: 'The server must stay local.',
  status: 'Accepted',
  timestamp: '2026-07-09T00:00:00Z',
  author: 'MOOSEDev',
  story_component_iri: null,
  outgoing: [
    {
      predicate: 'constrains',
      target_iri: 'https://moosedev.dev/kg/CodeEntity/record-2',
      target_label: 'HTTP server',
      target_kind: 'CodeEntity',
    },
  ],
  incoming: [],
};

describe('RecordPage', () => {
  it('renders record details and its relationship graph', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, json: async () => response }),
    );

    render(<RecordPage uuid="record-1" />);

    expect(await screen.findByText('Keep local operation')).toBeInTheDocument();
    expect(screen.getByText('Constraint')).toBeInTheDocument();
    expect(screen.getByText('Connections')).toBeInTheDocument();
    expect(screen.getByText('Relationship graph for Keep local operation')).toBeInTheDocument();
    expect(screen.queryByText('Outgoing')).not.toBeInTheDocument();
    expect(screen.getByText('The server must stay local.')).toBeInTheDocument();
  });

  it('renders an error alert when fetching fails', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('Not found')));

    render(<RecordPage uuid="missing" />);

    expect(await screen.findByText('Not found')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toBeInTheDocument();
  });

  it('forwards typed records to their generated artifact interface', async () => {
    const onResolveArtifact = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          ...response,
          iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-1',
          kind: 'ArchitecturalDecision',
        }),
      }),
    );

    render(<RecordPage uuid="adr-1" onResolveArtifact={onResolveArtifact} />);

    await waitFor(() => {
      expect(onResolveArtifact).toHaveBeenCalledWith({
        kind: 'adrs',
        iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-1',
      });
    });
    expect(screen.queryByText('Keep local operation')).not.toBeInTheDocument();
  });

  it('keeps typed evidence in generic record detail when artifact resolution is disabled', async () => {
    const onResolveArtifact = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          ...response,
          iri: 'https://moosedev.dev/kg/Requirement/req-1',
          kind: 'Requirement',
        }),
      }),
    );

    render(
      <RecordPage
        uuid="req-1"
        onResolveArtifact={onResolveArtifact}
        resolveArtifacts={false}
      />,
    );

    expect(await screen.findByText('Keep local operation')).toBeInTheDocument();
    expect(onResolveArtifact).not.toHaveBeenCalled();
  });

  it('launches a Story from a component record', async () => {
    const onTellStory = vi.fn();
    const component = {
      ...response,
      iri: 'https://moosedev.dev/kg/SystemComponent/graph',
      kind: 'SystemComponent',
      title: 'graph/store layer',
      story_component_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
    };
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, json: async () => component }),
    );

    render(<RecordPage uuid="graph" onTellStory={onTellStory} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Tell this Story' }));

    expect(onTellStory).toHaveBeenCalledWith(component.iri);
  });

  it('launches the uniquely concerned component Story from a knowledge record', async () => {
    const onTellStory = vi.fn();
    const concernedRecord = {
      ...response,
      story_component_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
      outgoing: [
        {
          predicate: 'concerns',
          target_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
          target_label: 'graph/store layer',
          target_kind: 'SystemComponent',
        },
      ],
    };
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, json: async () => concernedRecord }),
    );

    render(<RecordPage uuid="record-1" onTellStory={onTellStory} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Tell this Story' }));

    expect(onTellStory).toHaveBeenCalledWith('https://moosedev.dev/kg/SystemComponent/graph');
  });

  it('does not invent Story eligibility when the backend omits it', async () => {
    const onTellStory = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          ...response,
          outgoing: [
            {
              predicate: 'https://moosedev.dev/ontology#concerns',
              target_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
              target_label: 'graph/store layer',
              target_kind: 'SystemComponent',
            },
          ],
        }),
      }),
    );

    render(<RecordPage uuid="record-1" onTellStory={onTellStory} />);
    await screen.findByText('Keep local operation');
    expect(screen.queryByRole('button', { name: 'Tell this Story' })).not.toBeInTheDocument();
    expect(onTellStory).not.toHaveBeenCalled();
  });

  it('suppresses Story launch when the backend marks a component ineligible', async () => {
    const onTellStory = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          ...response,
          iri: 'https://moosedev.dev/kg/SystemComponent/old',
          kind: 'SystemComponent',
          status: 'superseded',
        }),
      }),
    );

    render(<RecordPage uuid="old" onTellStory={onTellStory} />);
    await screen.findByText('Keep local operation');

    expect(screen.queryByRole('button', { name: 'Tell this Story' })).not.toBeInTheDocument();
  });

  it('launches the component Story selected from inverse links by the backend', async () => {
    const onTellStory = vi.fn();
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({
          ...response,
          story_component_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
          incoming: [
            {
              predicate: 'isConcernedBy',
              source_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
              source_label: 'graph/store layer',
              source_kind: 'SystemComponent',
            },
          ],
        }),
      }),
    );

    render(<RecordPage uuid="record-1" onTellStory={onTellStory} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Tell this Story' }));

    expect(onTellStory).toHaveBeenCalledWith('https://moosedev.dev/kg/SystemComponent/graph');
  });
});
