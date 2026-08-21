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
  code: null,
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

const codeEntity = {
  ...response,
  iri: 'https://moosedev.dev/kg/CodeEntity/entity-1',
  kind: 'CodeEntity',
  title: 'build_routes',
  description: null,
  story_component_iri: 'https://moosedev.dev/kg/SystemComponent/http',
  code: {
    symbol: 'rust-analyzer cargo moosedev 0.9.0 api::routes::build_routes().',
    name: 'build_routes',
    entity_kind: 'Function',
    logical_path: 'api::routes::build_routes',
    defined_in_path: 'src/api/routes.rs',
    signature: 'pub fn build_routes(state: Arc<AppState>) -> Router',
    source_path: 'src/api/routes.rs',
    definition: { start_line: 10, start_col: 8, end_line: 10, end_col: 20 },
    source_available: true,
    source_unavailable_reason: null,
    substrate_stale: false,
  },
};

const contextSource = {
  path: 'src/api/routes.rs',
  scope: 'context',
  start_line: 9,
  end_line: 11,
  total_lines: 77,
  truncated: false,
  definition: { start_line: 10, start_col: 8, end_line: 10, end_col: 20 },
  text: 'use axum::Router;\npub fn build_routes() -> Router {\n    Router::new()',
};

const fullSource = {
  ...contextSource,
  scope: 'full',
  start_line: 1,
  end_line: 3,
  text: 'line one\nline two\nline three',
};

/**
 * Route a stubbed fetch so a page can load record metadata and source
 * independently. A scope with no fixture answers the way the daemon does when
 * it will not serve source: an error, not an empty body.
 */
function stubRoutes(routes: { record: unknown; context?: unknown; full?: unknown }) {
  const fetchMock = vi.fn(async (url: string) => {
    if (!url.includes('/source')) {
      return { ok: true, json: async () => routes.record };
    }
    const source = url.includes('scope=full') ? routes.full : routes.context;
    return source
      ? { ok: true, json: async () => source }
      : {
          ok: false,
          status: 503,
          statusText: 'Service Unavailable',
          json: async () => ({ error: 'unavailable' }),
        };
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

describe('RecordPage CodeEntity workbench', () => {
  it('renders code metadata and a line-numbered definition preview', async () => {
    stubRoutes({ record: codeEntity, context: contextSource });

    render(<RecordPage uuid="entity-1" />);

    expect(await screen.findByText('Code')).toBeInTheDocument();
    expect(screen.getByText('Function')).toBeInTheDocument();
    expect(screen.getByText('src/api/routes.rs:10')).toBeInTheDocument();
    expect(
      screen.getByText('pub fn build_routes(state: Arc<AppState>) -> Router'),
    ).toBeInTheDocument();

    // Line numbers continue from the window start, not from 1.
    expect(await screen.findByText('9')).toBeInTheDocument();
    expect(screen.getByText('10')).toBeInTheDocument();
    expect(screen.getByText('11')).toBeInTheDocument();
    expect(screen.getByText('use axum::Router;')).toBeInTheDocument();
    expect(screen.getByText('Lines 9–11 of 77')).toBeInTheDocument();

    // Only the definition line is marked.
    const definitionRow = screen.getByText('pub fn build_routes() -> Router {').closest('tr');
    expect(definitionRow).toHaveAttribute('data-definition', 'true');
    const contextRow = screen.getByText('use axum::Router;').closest('tr');
    expect(contextRow).not.toHaveAttribute('data-definition');
  });

  it('expands to the full indexed file on request', async () => {
    stubRoutes({ record: codeEntity, context: contextSource, full: fullSource });

    render(<RecordPage uuid="entity-1" />);
    fireEvent.click(await screen.findByRole('button', { name: 'Show full file' }));

    expect(await screen.findByText('line two')).toBeInTheDocument();
    expect(
      await screen.findByRole('button', { name: 'Show definition only' }),
    ).toBeInTheDocument();
  });

  it('keeps the working preview when a full-file expansion fails', async () => {
    // No `full` fixture, so that request errors. The context view must survive
    // it — discarding the loaded preview on a failed expansion would leave the
    // reader staring at an error where working source had been.
    stubRoutes({ record: codeEntity, context: contextSource });

    render(<RecordPage uuid="entity-1" />);
    fireEvent.click(await screen.findByRole('button', { name: 'Show full file' }));

    expect(await screen.findByText('unavailable')).toBeInTheDocument();
    // The definition preview is still on screen...
    expect(screen.getByText('pub fn build_routes() -> Router {')).toBeInTheDocument();
    // ...and the toggle is still there to get back to it.
    expect(
      await screen.findByRole('button', { name: 'Show definition only' }),
    ).toBeInTheDocument();
  });

  it('still offers the full file when the context window refuses the definition', async () => {
    // A definition longer than the 400-line context window is refused at
    // `scope=context` while `scope=full` serves it. No preview ever loads, so
    // gating the toggle on a loaded source would answer that failure by
    // removing the one control that recovers from it.
    stubRoutes({ record: codeEntity, full: fullSource });

    render(<RecordPage uuid="entity-1" />);

    expect(await screen.findByText('unavailable')).toBeInTheDocument();
    fireEvent.click(await screen.findByRole('button', { name: 'Show full file' }));

    expect(await screen.findByText('line two')).toBeInTheDocument();
  });

  it('explains an untrusted index instead of previewing source', async () => {
    stubRoutes({
      record: {
        ...codeEntity,
        code: {
          ...codeEntity.code,
          source_available: false,
          source_unavailable_reason:
            'The file on disk cannot be proven to match the indexed generation, so no source is shown. Re-run `moosedev index`.',
          substrate_stale: true,
        },
      },
    });

    render(<RecordPage uuid="entity-1" />);

    expect(await screen.findByText(/cannot be proven to match the indexed generation/)).toBeInTheDocument();
    expect(screen.getByText('Index is behind HEAD')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Show full file' })).not.toBeInTheDocument();
    // Metadata still answers "where does this live" even without a preview.
    expect(screen.getByText('src/api/routes.rs:10')).toBeInTheDocument();
  });

  it('surfaces a failed source read without hiding the record', async () => {
    stubRoutes({ record: codeEntity });

    render(<RecordPage uuid="entity-1" />);

    expect(await screen.findByText('unavailable')).toBeInTheDocument();
    expect(screen.getByText('build_routes')).toBeInTheDocument();
  });

  it('tells the Story of the exact entity, not its containing component', async () => {
    const onTellStory = vi.fn();
    stubRoutes({ record: codeEntity, context: contextSource });

    render(<RecordPage uuid="entity-1" onTellStory={onTellStory} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Tell this Story' }));

    expect(onTellStory).toHaveBeenCalledWith('https://moosedev.dev/kg/CodeEntity/entity-1');
    expect(onTellStory).not.toHaveBeenCalledWith('https://moosedev.dev/kg/SystemComponent/http');
  });

  it('highlights only the lines the definition actually covers', async () => {
    // A multi-line declaration whose exclusive end sits at column 1 of line 11
    // stops at the START of that line, so line 11 holds none of it.
    stubRoutes({
      record: {
        ...codeEntity,
        code: {
          ...codeEntity.code,
          definition: { start_line: 9, start_col: 1, end_line: 11, end_col: 1 },
        },
      },
      context: {
        ...contextSource,
        start_line: 9,
        end_line: 11,
        definition: { start_line: 9, start_col: 1, end_line: 11, end_col: 1 },
        text: 'pub fn build_routes() -> Router {\n    Router::new()\n}',
      },
    });

    render(<RecordPage uuid="entity-1" />);

    // Source lines keep their indentation, so query without normalization.
    const row = (text: string) =>
      screen.getByText(text, { normalizer: (value) => value }).closest('tr');

    await screen.findByText('pub fn build_routes() -> Router {');
    expect(row('pub fn build_routes() -> Router {')).toHaveAttribute('data-definition', 'true');
    expect(row('    Router::new()')).toHaveAttribute('data-definition', 'true');
    expect(row('}')).not.toHaveAttribute('data-definition');
  });

  it('leaves ordinary records without a code section', async () => {
    stubRoutes({ record: response });

    render(<RecordPage uuid="record-1" />);

    expect(await screen.findByText('Keep local operation')).toBeInTheDocument();
    expect(screen.queryByText('Code')).not.toBeInTheDocument();
  });
});
