// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { api } from '../api/client';
import { StoryCheckGradeResponse, StoryRecipe, StoryRun } from '../api/types';
import StoriesPage from './StoriesPage';

vi.mock('../api/client', () => ({
  api: {
    listStories: vi.fn(),
    getStory: vi.fn(),
    generateStory: vi.fn(),
    saveStory: vi.fn(),
    publishStory: vi.fn(),
    gradeStoryCheck: vi.fn(),
  },
}));

const recipe: StoryRecipe = {
  id: 'graph-store',
  title: 'The graph store',
  subject_component_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
  goal: 'Understand the graph boundary.',
  audience: 'reboarding',
  status: 'draft',
  curator: 'James',
  updated_at: '2026-08-13T20:00:00Z',
  beats: [
    { id: 'purpose', title: 'Why it exists', intent: 'purpose', record_iris: ['record-1'], code_symbols: [] },
    { id: 'boundary', title: 'Its boundary', intent: 'boundary', record_iris: [], code_symbols: ['symbol boundary'] },
    { id: 'risk', title: 'What to protect', intent: 'risk', record_iris: ['record-risk'], code_symbols: [] },
  ],
};

const run: StoryRun = {
  recipe_id: null,
  trust_state: 'generated',
  narration_mode: 'symbolic',
  title: 'The graph store',
  subject: { iri: recipe.subject_component_iri, label: 'graph/store layer' },
  goal: recipe.goal,
  overview: 'The graph store owns authoritative project knowledge.',
  beats: [
    {
      id: 'purpose',
      title: 'Why it exists',
      intent: 'purpose',
      narrative: 'It keeps typed project knowledge queryable.',
      evidence: [
        {
          iri: 'https://moosedev.dev/kg/Requirement/one',
          title: 'Queryable memory',
          kind: 'Requirement',
          status: 'accepted',
        },
      ],
      code_anchors: [
        {
          symbol: 'scip symbol graph-store',
          label: 'GraphStore',
          entity_iri: 'https://moosedev.dev/kg/CodeEntity/store',
          path: 'src/graph/store.rs',
          line: 0,
        },
      ],
    },
    {
      id: 'boundary',
      title: 'Its boundary',
      intent: 'boundary',
      narrative: 'The HTTP layer remains a thin client.',
      evidence: [],
      code_anchors: [],
      gap: 'No accepted record explains transaction ownership.',
    },
    {
      id: 'risk',
      title: 'What to protect',
      intent: 'risk',
      narrative: 'Do not create a second source of truth.',
      evidence: [],
      code_anchors: [],
    },
  ],
  gaps: [{ id: 'gap-1', title: 'Transaction ownership', detail: 'No accepted rationale is linked.' }],
  checks: [
    {
      id: 'check-1',
      question: 'What is the authoritative source?',
      options: [
        { id: 'graph', label: 'The project knowledge graph' },
        { id: 'story', label: 'The Story recipe' },
      ],
    },
  ],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: run });
  vi.mocked(api.listStories).mockResolvedValue({
    stories: [
      {
        id: recipe.id,
        title: recipe.title,
        subject_component_iri: recipe.subject_component_iri,
        subject_label: 'graph/store layer',
        goal: recipe.goal,
        audience: 'reboarding',
        status: 'draft',
        curator: recipe.curator,
        beat_count: 3,
      },
    ],
  });
  vi.mocked(api.getStory).mockResolvedValue({ recipe });
  vi.mocked(api.saveStory).mockImplementation(async (value) => ({
    recipe: { ...value, updated_at: '2026-08-13T20:01:00Z' },
  }));
  vi.mocked(api.publishStory).mockResolvedValue({ recipe: { ...recipe, status: 'published' } });
  vi.mocked(api.gradeStoryCheck).mockResolvedValue({
    correct: true,
    feedback: 'Correct — the accepted Requirement establishes this.',
    evidence_iris: ['https://moosedev.dev/kg/Requirement/one'],
  });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('StoriesPage', () => {
  it('immediately generates from a contextual component launch', async () => {
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: run });

    render(
      <StoriesPage
        onNavigateRecord={vi.fn()}
        initialComponentIri="https://moosedev.dev/kg/SystemComponent/graph"
      />,
    );

    await waitFor(() => {
      expect(api.generateStory).toHaveBeenCalledWith({
        component_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
        assist_level: 1,
        include_checks: false,
      });
    });
    expect(await screen.findByText('Symbolic extract')).toBeInTheDocument();
  });

  it('regenerates a curated Story fresh from its subject', async () => {
    const publishedRun = { ...run, recipe_id: 'graph-store', trust_state: 'published' as const };
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: publishedRun })
      .mockResolvedValueOnce({ outcome: 'story', story: publishedRun })
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockResolvedValueOnce({ outcome: 'story', story: run });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByText('The graph store'));
    expect(await screen.findByText('Published Story')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Generate fresh' }));

    await waitFor(() => {
      expect(api.generateStory).toHaveBeenLastCalledWith({
        component_iri: recipe.subject_component_iri,
        fresh: true,
        assist_level: 1,
        include_checks: false,
      });
    });
    expect(await screen.findByText('Generated Story')).toBeInTheDocument();
  });

  it('resolves an ambiguous prompt before rendering a Story', async () => {
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({
        outcome: 'ambiguous',
        prompt: 'graph',
        recipe_id: 'graph-store',
        candidates: [
          { iri: 'component-graph', label: 'Graph store' },
          { iri: 'component-code', label: 'Code graph' },
        ],
      })
      .mockResolvedValueOnce({ outcome: 'story', story: run });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));

    expect(await screen.findByText('Which subject did you mean?')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Graph store' }));

    await waitFor(() => {
      expect(api.generateStory).toHaveBeenLastCalledWith({
        prompt: 'graph',
        recipe_id: 'graph-store',
        component_iri: 'component-graph',
        assist_level: 1,
        include_checks: false,
      });
    });
    expect(await screen.findByText('Symbolic extract')).toBeInTheDocument();
  });

  it('issues only one symbolic generation for synchronous duplicate submits', async () => {
    const symbolic = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory).mockReturnValueOnce(symbolic.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), {
      target: { value: 'graph store' },
    });
    const form = screen.getByRole('button', { name: 'Tell Story' }).closest('form');
    expect(form).not.toBeNull();

    fireEvent.submit(form!);
    fireEvent.submit(form!);

    expect(api.generateStory).toHaveBeenCalledTimes(1);
    await act(async () => {
      symbolic.resolve({ outcome: 'story', story: run });
    });
  });

  it('keeps evidence, code anchors, gaps, and graph-graded checks visible', async () => {
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: run });
    const navigate = vi.fn();
    render(<StoriesPage onNavigateRecord={navigate} />);

    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));

    fireEvent.click(await screen.findByText('Requirement: Queryable memory · accepted'));
    expect(navigate).toHaveBeenCalledWith('https://moosedev.dev/kg/Requirement/one');
    fireEvent.click(screen.getByText(/GraphStore/));
    expect(navigate).toHaveBeenCalledWith('https://moosedev.dev/kg/CodeEntity/store');
    expect(screen.getByText('GraphStore · src/graph/store.rs:0')).toBeInTheDocument();
    expect(screen.getByText(/No accepted record explains transaction ownership/)).toBeInTheDocument();
    expect(screen.getByText('This Story cannot currently answer everything')).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText('The project knowledge graph'));
    fireEvent.click(screen.getByRole('button', { name: 'Check answer' }));
    expect(await screen.findByText(/Correct —/)).toBeInTheDocument();
    expect(api.gradeStoryCheck).toHaveBeenCalledWith({
      check_id: 'check-1',
      selected_option_ids: ['graph'],
    });
  });

  it('shows lifecycle status and styles every backend working-set status as current', async () => {
    const lifecycleRun: StoryRun = {
      ...run,
      beats: [
        {
          ...run.beats[0],
          evidence: [
            { ...run.beats[0].evidence[0], status: 'implemented' },
            {
              iri: run.subject.iri,
              title: run.subject.label,
              kind: 'SystemComponent',
              status: 'superseded',
            },
          ],
        },
        ...run.beats.slice(1),
      ],
    };
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: lifecycleRun });
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));

    const current = await screen.findByText('Requirement: Queryable memory · implemented');
    const retired = screen.getByText('SystemComponent: graph/store layer · superseded');
    expect(current.closest('.MuiChip-root')).toHaveClass('MuiChip-colorPrimary');
    expect(retired.closest('.MuiChip-root')).toHaveClass('MuiChip-colorWarning');
  });

  it('surfaces grading failures without leaving the check busy', async () => {
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: run });
    vi.mocked(api.gradeStoryCheck).mockRejectedValueOnce(new Error('Unable to grade against current graph'));
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByLabelText('The project knowledge graph'));
    fireEvent.click(screen.getByRole('button', { name: 'Check answer' }));

    expect(await screen.findByText('Unable to grade against current graph')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Check answer' })).toBeEnabled();
  });

  it('saves generated Stories as reference-only draft recipes', async () => {
    const reloadedRun = {
      ...run,
      recipe_id: 'graph-store',
      trust_state: 'draft' as const,
      title: 'Server-resolved graph Story',
      beats: [
        {
          ...run.beats[0],
          evidence: [
            {
              iri: 'https://moosedev.dev/kg/Constraint/server',
              title: 'Server-selected evidence',
              kind: 'Constraint',
              status: 'accepted',
            },
          ],
        },
        ...run.beats.slice(1),
      ],
    };
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockResolvedValueOnce({ outcome: 'story', story: reloadedRun });
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Save as draft' }));

    await waitFor(() => expect(api.saveStory).toHaveBeenCalled());
    const saved = vi.mocked(api.saveStory).mock.calls[0][0];
    expect(saved.status).toBe('draft');
    expect(saved.beats[0]).toMatchObject({
      record_iris: ['https://moosedev.dev/kg/Requirement/one'],
      code_symbols: ['scip symbol graph-store'],
    });
    expect(saved).not.toHaveProperty('overview');
    expect(api.generateStory).toHaveBeenLastCalledWith({ recipe_id: saved.id, assist_level: 1 });
    expect(await screen.findByText('Server-resolved graph Story')).toBeInTheDocument();
    expect(screen.getByText('Constraint: Server-selected evidence · accepted')).toBeInTheDocument();
    expect(screen.queryByText('Requirement: Queryable memory · accepted')).not.toBeInTheDocument();
  });

  it('supports adding, removing, reordering, annotating, saving, and publishing beats', async () => {
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    expect(await screen.findByText('Curate Story')).toBeInTheDocument();

    fireEvent.change(screen.getAllByLabelText('Curator note')[0], { target: { value: 'Start with the requirement.' } });
    fireEvent.change(screen.getByLabelText('Record IRIs for What to protect'), { target: { value: '' } });
    expect(screen.getByRole('button', { name: 'Publish' })).toBeDisabled();
    expect(screen.getByText(/Every published beat except Boundary needs at least one record IRI/)).toHaveTextContent('What to protect');
    fireEvent.change(screen.getByLabelText('Record IRIs for What to protect'), {
      target: { value: 'https://moosedev.dev/kg/Constraint/one,\n https://moosedev.dev/kg/Requirement/two' },
    });
    expect(screen.getByRole('button', { name: 'Publish' })).toBeEnabled();
    fireEvent.click(screen.getByLabelText('Move Why it exists down'));
    fireEvent.click(screen.getByRole('button', { name: 'Add beat' }));
    fireEvent.click(screen.getByLabelText('Remove New beat'));
    fireEvent.click(screen.getByRole('button', { name: 'Save draft' }));

    await waitFor(() => expect(api.saveStory).toHaveBeenCalled());
    const saved = vi.mocked(api.saveStory).mock.calls[0][0];
    expect(saved.beats.map((beat) => beat.id)).toEqual(['boundary', 'purpose', 'risk']);
    expect(saved.beats[1].curator_note).toBe('Start with the requirement.');
    expect(saved.beats[2].record_iris).toEqual([
      'https://moosedev.dev/kg/Constraint/one',
      'https://moosedev.dev/kg/Requirement/two',
    ]);

    fireEvent.click(screen.getByLabelText('Move Its boundary down'));
    fireEvent.click(screen.getByRole('button', { name: 'Publish' }));
    await waitFor(() =>
      expect(api.publishStory).toHaveBeenCalledWith(
        'graph-store',
        '2026-08-13T20:01:00Z',
      ),
    );
  });

  it('allows zero-to-five-beat drafts but blocks saving above the backend cap', async () => {
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));

    fireEvent.click(await screen.findByRole('button', { name: 'Add beat' }));
    fireEvent.click(screen.getByRole('button', { name: 'Add beat' }));
    expect(screen.getByRole('button', { name: 'Save draft' })).toBeEnabled();

    fireEvent.click(screen.getByRole('button', { name: 'Add beat' }));
    expect(screen.getByText('Stories may contain at most five beats. Remove a beat before saving.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save draft' })).toBeDisabled();

    for (let index = 0; index < 6; index += 1) {
      fireEvent.click(screen.getAllByRole('button', { name: /^Remove / })[0]);
    }
    expect(screen.getByText('Drafts may contain zero to five beats; publishing requires at least three.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save draft' })).toBeEnabled();
    expect(screen.getByRole('button', { name: 'Publish' })).toBeDisabled();
  });

  it('preserves published status, protects dirty edits, and refreshes the reader after saving', async () => {
    const publishedRecipe = { ...recipe, status: 'published' as const };
    const refreshedRun = {
      ...run,
      recipe_id: recipe.id,
      trust_state: 'published' as const,
      title: 'Updated graph story',
    };
    vi.mocked(api.getStory).mockResolvedValueOnce({ recipe: publishedRecipe });
    vi.mocked(api.saveStory).mockImplementationOnce(async (value) => ({ recipe: value }));
    vi.mocked(api.generateStory).mockResolvedValueOnce({ outcome: 'story', story: refreshedRun });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    fireEvent.change(await screen.findByLabelText('Title'), { target: { value: 'Updated graph story' } });

    expect(screen.getByLabelText('Tell me the story of…')).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Tell Story' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Close' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(api.saveStory).toHaveBeenCalledWith(expect.objectContaining({
        title: 'Updated graph story',
        status: 'published',
      }));
      expect(api.generateStory).toHaveBeenCalledWith({ recipe_id: recipe.id, assist_level: 1 });
    });
    expect(screen.getByRole('button', { name: 'Close' })).toBeEnabled();
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(await screen.findByText('Updated graph story')).toBeInTheDocument();
    expect(screen.getByText('Published Story')).toBeInTheDocument();
  });

  it('rejects excessive and duplicate per-beat references before saving', async () => {
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));

    fireEvent.change(await screen.findByLabelText('Record IRIs for Why it exists'), {
      target: { value: 'one\ntwo\nthree\nfour\nfive\nsix\nseven' },
    });
    fireEvent.change(screen.getByLabelText('Code symbols for Its boundary'), {
      target: { value: 'duplicate\nduplicate' },
    });
    fireEvent.change(screen.getByLabelText('Record IRIs for What to protect'), {
      target: { value: 'record-risk\nrecord-risk' },
    });
    fireEvent.change(screen.getByLabelText('Code symbols for Why it exists'), {
      target: { value: 'one\ntwo\nthree\nfour\nfive\nsix\nseven' },
    });

    expect(screen.getByText(/at most six unique records and six unique code symbols/)).toHaveTextContent(
      'Why it exists has more than six record IRIs',
    );
    expect(screen.getByText(/at most six unique records and six unique code symbols/)).toHaveTextContent(
      'Its boundary has duplicate code symbols',
    );
    expect(screen.getByText(/at most six unique records and six unique code symbols/)).toHaveTextContent(
      'What to protect has duplicate record IRIs',
    );
    expect(screen.getByText(/at most six unique records and six unique code symbols/)).toHaveTextContent(
      'Why it exists has more than six code symbols',
    );
    expect(screen.getByRole('button', { name: 'Save draft' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Publish' })).toBeDisabled();
  });

  it('busy-gates ambiguity choices and reader actions', async () => {
    const candidateRequest = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({
        outcome: 'ambiguous',
        prompt: 'graph',
        candidates: [
          { iri: 'component-graph', label: 'Graph store' },
          { iri: 'component-code', label: 'Code graph' },
        ],
      })
      .mockReturnValueOnce(candidateRequest.promise)
      .mockResolvedValueOnce({
        outcome: 'story',
        story: { ...run, recipe_id: recipe.id, trust_state: 'published' },
      });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Graph store' }));
    expect(screen.getByRole('button', { name: 'Graph store' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Code graph' })).toBeDisabled();

    candidateRequest.resolve({
      outcome: 'story',
      story: { ...run, recipe_id: recipe.id, trust_state: 'published' },
    });
    expect(await screen.findByRole('button', { name: 'Generate fresh' })).toBeEnabled();

    const freshRequest = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory).mockReturnValueOnce(freshRequest.promise);
    fireEvent.click(screen.getByRole('button', { name: 'Generate fresh' }));
    expect(screen.getByRole('button', { name: 'All Stories' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Generate fresh' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Curate' })).toBeDisabled();
    freshRequest.resolve({ outcome: 'story', story: run });
  });

  it('does not publish when the save response omits its CAS token', async () => {
    vi.mocked(api.saveStory).mockImplementationOnce(async (value) => ({
      recipe: { ...value, updated_at: null },
    }));

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Publish' }));

    expect(await screen.findByText('Story changes were saved, but the server did not return the updated_at token required to publish')).toBeInTheDocument();
    expect(api.publishStory).not.toHaveBeenCalled();
  });

  it('advances the editor CAS baseline when publish fails after a successful save', async () => {
    vi.mocked(api.saveStory).mockImplementationOnce(async (value) => ({
      recipe: { ...value, updated_at: '2026-08-13T21:00:00Z' },
    }));
    vi.mocked(api.publishStory).mockRejectedValueOnce(new Error('Story changed concurrently'));

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    fireEvent.change(await screen.findByLabelText('Title'), { target: { value: 'Saved before publish' } });
    fireEvent.click(screen.getByRole('button', { name: 'Publish' }));

    expect(await screen.findByText(/Story changes were saved, but publication failed: Story changed concurrently/)).toBeInTheDocument();
    expect(api.publishStory).toHaveBeenCalledWith('graph-store', '2026-08-13T21:00:00Z');
    expect(screen.getByRole('button', { name: 'Close' })).toBeEnabled();
    expect(screen.queryByText('Save changes before closing or starting another Story.')).not.toBeInTheDocument();
  });

  it('keeps a successful server reload visible when library refresh fails', async () => {
    const serverRun = { ...run, recipe_id: recipe.id, trust_state: 'draft' as const, title: 'Reload succeeded' };
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockResolvedValueOnce({ outcome: 'story', story: serverRun });
    vi.mocked(api.listStories)
      .mockResolvedValueOnce({ stories: [] })
      .mockRejectedValueOnce(new Error('Library unavailable'));

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Save as draft' }));

    expect(await screen.findByText('Reload succeeded')).toBeInTheDocument();
    expect(screen.getByText(/Story was saved as draft, but the library could not be refreshed: Library unavailable/)).toBeInTheDocument();
  });

  it('mints collision-free beat IDs when UUID generation is unavailable and timestamps collide', async () => {
    const originalCrypto = globalThis.crypto;
    vi.stubGlobal('crypto', {});
    vi.spyOn(Date, 'now').mockReturnValue(12345);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Add beat' }));
    fireEvent.click(screen.getByRole('button', { name: 'Add beat' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save draft' }));

    await waitFor(() => expect(api.saveStory).toHaveBeenCalled());
    const saved = vi.mocked(api.saveStory).mock.calls[0][0];
    const ids = saved.beats.map((beat) => beat.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(ids.filter((id) => id.startsWith('beat-')).length).toBe(2);
    vi.stubGlobal('crypto', originalCrypto);
  });

  it('renders symbolically first and ignores assisted narration after navigation', async () => {
    const assisted = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockReturnValueOnce(assisted.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));

    expect(await screen.findByText('Symbolic extract')).toBeInTheDocument();
    expect(screen.getByText('Improving narration…')).toBeInTheDocument();
    expect(api.generateStory).toHaveBeenNthCalledWith(1, { prompt: 'graph store', assist_level: 0 });
    expect(api.generateStory).toHaveBeenNthCalledWith(2, { prompt: 'graph store', assist_level: 1, include_checks: false });

    fireEvent.click(screen.getByRole('button', { name: 'All Stories' }));
    assisted.resolve({
      outcome: 'story',
      story: { ...run, narration_mode: 'llm', title: 'Late assisted Story' },
    });

    await waitFor(() => expect(screen.queryByText('Late assisted Story')).not.toBeInTheDocument());
    expect(screen.getByText('Saved Stories')).toBeInTheDocument();
  });

  it('ignores a stale assisted error after navigation', async () => {
    const assisted = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockReturnValueOnce(assisted.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByRole('button', { name: 'All Stories' }));
    assisted.reject(new Error('Late LLM failure'));

    await waitFor(() => expect(screen.queryByText(/Late LLM failure/)).not.toBeInTheDocument());
    expect(screen.getByText('Saved Stories')).toBeInTheDocument();
  });

  it('does not let pending assistance overwrite a generated Story save', async () => {
    const assisted = deferred<{ outcome: 'story'; story: StoryRun }>();
    const savedRun = {
      ...run,
      recipe_id: recipe.id,
      trust_state: 'draft' as const,
      title: 'Saved server Story',
    };
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockReturnValueOnce(assisted.promise)
      .mockResolvedValueOnce({ outcome: 'story', story: savedRun });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Save as draft' }));
    expect(await screen.findByText('Saved server Story')).toBeInTheDocument();

    assisted.resolve({
      outcome: 'story',
      story: { ...run, narration_mode: 'llm', title: 'Stale assisted Story' },
    });
    await waitFor(() => expect(screen.queryByText('Stale assisted Story')).not.toBeInTheDocument());
    expect(screen.getByText('Saved server Story')).toBeInTheDocument();
  });

  it('preserves quiz answers across a symbolic-to-assisted presentation upgrade', async () => {
    const assisted = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockReturnValueOnce(assisted.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    const answer = await screen.findByLabelText('The project knowledge graph');
    fireEvent.click(answer);
    expect(answer).toBeChecked();

    assisted.resolve({
      outcome: 'story',
      story: {
        ...run,
        narration_mode: 'llm',
        beats: run.beats.map((beat) => ({
          ...beat,
          narrative: `Assisted: ${beat.narrative}`,
        })),
        checks: [
          {
            id: 'random-assisted-check-id',
            question: 'This presentation-only quiz must be ignored',
            options: [{ id: 'ignored', label: 'Ignored option' }],
          },
        ],
      },
    });

    expect(await screen.findByText('LLM-assisted narration')).toBeInTheDocument();
    expect(screen.getByText('Assisted: It keeps typed project knowledge queryable.')).toBeInTheDocument();
    expect(screen.getByLabelText('The project knowledge graph')).toBeChecked();
    expect(screen.queryByText('This presentation-only quiz must be ignored')).not.toBeInTheDocument();
  });

  it('rejects an assisted snapshot whose title or evidence differs from the symbolic structure', async () => {
    const assisted = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockReturnValueOnce(assisted.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    expect(await screen.findByText('The graph store')).toBeInTheDocument();

    assisted.resolve({
      outcome: 'story',
      story: {
        ...run,
        narration_mode: 'llm',
        title: 'Structurally different Story',
        beats: [
          {
            ...run.beats[0],
            narrative: 'Assisted replacement must not land.',
            evidence: [
              {
                iri: 'https://moosedev.dev/kg/Constraint/different',
                title: 'Different evidence',
                kind: 'Constraint',
                status: 'accepted',
              },
            ],
          },
          ...run.beats.slice(1),
        ],
      },
    });

    expect(await screen.findByText('Assisted narration did not match the symbolic Story structure; showing the symbolic Story.')).toBeInTheDocument();
    expect(screen.getByText('The graph store')).toBeInTheDocument();
    expect(screen.getByText('It keeps typed project knowledge queryable.')).toBeInTheDocument();
    expect(screen.queryByText('Structurally different Story')).not.toBeInTheDocument();
    expect(screen.queryByText('Assisted replacement must not land.')).not.toBeInTheDocument();
  });

  it('rejects assisted narration when visible evidence metadata changes', async () => {
    const assisted = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockReturnValueOnce(assisted.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    expect(await screen.findByText('The graph store')).toBeInTheDocument();

    assisted.resolve({
      outcome: 'story',
      story: {
        ...run,
        narration_mode: 'llm',
        beats: [
          {
            ...run.beats[0],
            narrative: 'Metadata drift must prevent this narration from landing.',
            evidence: [{ ...run.beats[0].evidence[0], title: 'Changed visible evidence title' }],
          },
          ...run.beats.slice(1),
        ],
      },
    });

    expect(await screen.findByText('Assisted narration did not match the symbolic Story structure; showing the symbolic Story.')).toBeInTheDocument();
    expect(screen.getByText('Requirement: Queryable memory · accepted')).toBeInTheDocument();
    expect(screen.queryByText('Changed visible evidence title')).not.toBeInTheDocument();
    expect(screen.queryByText('Metadata drift must prevent this narration from landing.')).not.toBeInTheDocument();
  });

  it('grades different checks concurrently with independent request identity', async () => {
    const firstGrade = deferred<StoryCheckGradeResponse>();
    const secondGrade = deferred<StoryCheckGradeResponse>();
    const twoCheckRun: StoryRun = {
      ...run,
      checks: [
        ...run.checks,
        {
          id: 'check-2',
          question: 'Which layer presents the Story?',
          options: [
            { id: 'ui', label: 'The web UI' },
            { id: 'store', label: 'The graph store' },
          ],
        },
      ],
    };
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: twoCheckRun });
    vi.mocked(api.gradeStoryCheck)
      .mockReturnValueOnce(firstGrade.promise)
      .mockReturnValueOnce(secondGrade.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByLabelText('The project knowledge graph'));
    fireEvent.click(screen.getByLabelText('The web UI'));
    const buttons = screen.getAllByRole('button', { name: 'Check answer' });
    fireEvent.click(buttons[0]);
    fireEvent.click(buttons[1]);

    expect(api.gradeStoryCheck).toHaveBeenCalledTimes(2);
    expect(buttons[0]).toBeDisabled();
    expect(buttons[1]).toBeDisabled();
    secondGrade.resolve({ correct: true, feedback: 'Second complete', evidence_iris: [] });
    expect(await screen.findByText('Second complete')).toBeInTheDocument();
    expect(screen.getAllByRole('button', { name: 'Check answer' })[0]).toBeDisabled();
    expect(screen.getAllByRole('button', { name: 'Check answer' })[1]).toBeEnabled();
    firstGrade.resolve({ correct: true, feedback: 'First complete', evidence_iris: [] });
    expect(await screen.findByText('First complete')).toBeInTheDocument();
  });

  it('guards Save as draft synchronously and mints its recipe ID with UUID entropy', async () => {
    const save = deferred<{ recipe: StoryRecipe }>();
    vi.stubGlobal('crypto', { randomUUID: () => '123e4567-e89b-12d3-a456-426614174000' });
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: run });
    vi.mocked(api.saveStory).mockReturnValue(save.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    const saveButton = await screen.findByRole('button', { name: 'Save as draft' });
    act(() => {
      saveButton.click();
      saveButton.click();
    });

    expect(api.saveStory).toHaveBeenCalledTimes(1);
    const savedRecipe = vi.mocked(api.saveStory).mock.calls[0][0];
    expect(savedRecipe.id).toBe('the-graph-store-123e4567-e89b-12d3-a456-426614174000');
    save.resolve({ recipe: savedRecipe });
    await waitFor(() => expect(api.generateStory).toHaveBeenCalledWith({ recipe_id: savedRecipe.id, assist_level: 1 }));
  });

  it('freezes editor mutations and synchronously guards save operations', async () => {
    const save = deferred<{ recipe: StoryRecipe }>();
    vi.mocked(api.saveStory).mockReturnValue(save.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    const saveButton = await screen.findByRole('button', { name: 'Save draft' });
    act(() => {
      saveButton.click();
      saveButton.click();
    });

    expect(api.saveStory).toHaveBeenCalledTimes(1);
    expect(screen.getByLabelText('Title')).toBeDisabled();
    expect(screen.getByLabelText('Learning goal')).toBeDisabled();
    expect(screen.getByLabelText('Beat 1')).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Add beat' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Remove Why it exists' })).toBeDisabled();

    save.resolve({ recipe: { ...recipe, updated_at: '2026-08-14T01:00:00Z' } });
    await waitFor(() => expect(screen.getByLabelText('Title')).toBeEnabled());
  });

  it('invalidates grading when an answer changes and clears feedback tied to the old selection', async () => {
    const firstGrade = deferred<StoryCheckGradeResponse>();
    vi.mocked(api.gradeStoryCheck)
      .mockReturnValueOnce(firstGrade.promise)
      .mockResolvedValueOnce({ correct: false, feedback: 'Second answer feedback', evidence_iris: [] });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByLabelText('The project knowledge graph'));
    fireEvent.click(screen.getByRole('button', { name: 'Check answer' }));
    fireEvent.click(screen.getByLabelText('The Story recipe'));

    expect(screen.getByRole('button', { name: 'Check answer' })).toBeEnabled();
    firstGrade.resolve({ correct: true, feedback: 'Feedback for the old answer', evidence_iris: [] });
    await waitFor(() => expect(screen.queryByText('Feedback for the old answer')).not.toBeInTheDocument());

    fireEvent.click(screen.getByRole('button', { name: 'Check answer' }));
    expect(await screen.findByText('Second answer feedback')).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText('The project knowledge graph'));
    expect(screen.queryByText('Second answer feedback')).not.toBeInTheDocument();
  });

  it('adopts a successfully saved draft before reload so a reload failure cannot create a duplicate', async () => {
    const generatedWithComponentBoundary: StoryRun = {
      ...run,
      beats: run.beats.map((beat) =>
        beat.intent === 'boundary'
          ? {
              ...beat,
              evidence: [{ iri: run.subject.iri, title: run.subject.label, kind: 'SystemComponent', status: 'accepted' }],
              code_anchors: [],
            }
          : beat,
      ),
    };
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: generatedWithComponentBoundary })
      .mockResolvedValueOnce({ outcome: 'story', story: generatedWithComponentBoundary })
      .mockRejectedValueOnce(new Error('Reader reload unavailable'));
    vi.mocked(api.saveStory).mockImplementationOnce(async (value) => ({
      recipe: { ...value, id: 'saved-once', updated_at: '2026-08-14T01:00:00Z' },
    }));

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Save as draft' }));

    expect(await screen.findByText(/Story was saved as draft, but its reader could not be reloaded: Reader reload unavailable/)).toBeInTheDocument();
    expect(screen.getByText('Draft Story')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Save as draft' })).not.toBeInTheDocument();
    expect(api.saveStory).toHaveBeenCalledTimes(1);
    const saved = vi.mocked(api.saveStory).mock.calls[0][0];
    expect(saved.beats.find((beat) => beat.intent === 'boundary')?.record_iris).toEqual([]);
  });

  it('keeps successful curated persistence honest when reader reload fails', async () => {
    const updatedTitle = 'Persisted without a reader reload';
    vi.mocked(api.saveStory).mockImplementationOnce(async (value) => ({
      recipe: { ...value, updated_at: '2026-08-14T01:00:00Z' },
    }));
    vi.mocked(api.generateStory).mockRejectedValueOnce(new Error('Reader unavailable'));
    vi.mocked(api.listStories)
      .mockResolvedValueOnce({
        stories: [
          {
            id: recipe.id,
            title: recipe.title,
            subject_component_iri: recipe.subject_component_iri,
            subject_label: 'graph/store layer',
            goal: recipe.goal,
            audience: 'reboarding',
            status: 'draft',
            curator: recipe.curator,
            beat_count: 3,
          },
        ],
      })
      .mockResolvedValueOnce({
        stories: [
          {
            id: recipe.id,
            title: updatedTitle,
            subject_component_iri: recipe.subject_component_iri,
            subject_label: 'graph/store layer',
            goal: recipe.goal,
            audience: 'reboarding',
            status: 'draft',
            curator: recipe.curator,
            beat_count: 3,
          },
        ],
      });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    fireEvent.change(await screen.findByLabelText('Title'), { target: { value: updatedTitle } });
    fireEvent.click(screen.getByRole('button', { name: 'Save draft' }));

    expect(await screen.findByText(/Story was saved, but its reader could not be reloaded: Reader unavailable/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(await screen.findByText(updatedTitle)).toBeInTheDocument();
    expect(screen.queryByText('Symbolic extract')).not.toBeInTheDocument();
  });

  it('keeps successful publication honest when reader reload fails', async () => {
    const published = {
      ...recipe,
      status: 'published' as const,
      updated_at: '2026-08-14T02:00:00Z',
    };
    vi.mocked(api.publishStory).mockResolvedValueOnce({ recipe: published });
    vi.mocked(api.generateStory).mockRejectedValueOnce(new Error('Published reader unavailable'));
    vi.mocked(api.listStories)
      .mockResolvedValueOnce({
        stories: [
          {
            id: recipe.id,
            title: recipe.title,
            subject_component_iri: recipe.subject_component_iri,
            subject_label: 'graph/store layer',
            goal: recipe.goal,
            audience: 'reboarding',
            status: 'draft',
            curator: recipe.curator,
            beat_count: 3,
          },
        ],
      })
      .mockResolvedValueOnce({
        stories: [
          {
            id: recipe.id,
            title: recipe.title,
            subject_component_iri: recipe.subject_component_iri,
            subject_label: 'graph/store layer',
            goal: recipe.goal,
            audience: 'reboarding',
            status: 'published',
            curator: recipe.curator,
            beat_count: 3,
          },
        ],
      });

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Publish' }));

    expect(await screen.findByText(/Story was published, but its reader could not be reloaded: Published reader unavailable/)).toBeInTheDocument();
    expect(api.publishStory).toHaveBeenCalledWith(recipe.id, '2026-08-13T20:01:00Z');
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.getByText('Saved Stories')).toBeInTheDocument();
    expect(screen.getByText('Published Story')).toBeInTheDocument();
    expect(screen.queryByText('Symbolic extract')).not.toBeInTheDocument();
  });

  it('busy-gates library curation synchronously', async () => {
    const load = deferred<{ recipe: StoryRecipe }>();
    vi.mocked(api.getStory).mockReturnValue(load.promise);

    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    const curate = await screen.findByRole('button', { name: 'Curate' });
    act(() => {
      curate.click();
      curate.click();
    });

    expect(api.getStory).toHaveBeenCalledTimes(1);
    expect(curate).toBeDisabled();
    load.resolve({ recipe });
    expect(await screen.findByText('Curate Story')).toBeInTheDocument();
  });

  it('reports dirty editor state to the App navigation boundary', async () => {
    const onDirtyChange = vi.fn();
    render(<StoriesPage onNavigateRecord={vi.fn()} onDirtyChange={onDirtyChange} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    fireEvent.change(await screen.findByLabelText('Title'), { target: { value: 'Unsaved Story title' } });
    await waitFor(() => expect(onDirtyChange).toHaveBeenLastCalledWith(true));
  });

  it('applies published anchor and canonical-intent validation to Save changes', async () => {
    vi.mocked(api.getStory).mockResolvedValueOnce({ recipe: { ...recipe, status: 'published' } });
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));

    fireEvent.change(await screen.findByLabelText('Code symbols for Its boundary'), { target: { value: '' } });
    expect(screen.getByRole('button', { name: 'Save changes' })).toBeEnabled();
    fireEvent.change(screen.getByLabelText('Record IRIs for What to protect'), { target: { value: '' } });
    expect(screen.getByRole('button', { name: 'Save changes' })).toBeDisabled();
    fireEvent.change(screen.getByLabelText('Record IRIs for What to protect'), { target: { value: 'record-risk' } });
    expect(screen.getByRole('button', { name: 'Save changes' })).toBeEnabled();
    fireEvent.click(screen.getByLabelText('Move Why it exists down'));
    expect(screen.getByRole('button', { name: 'Save changes' })).toBeDisabled();
    expect(screen.getByText(/Published Story beats must use unique intents in this order/)).toBeInTheDocument();
  });

  it('renders curator notes separately from authoritative narration', async () => {
    const notedRun: StoryRun = {
      ...run,
      beats: run.beats.map((beat, index) =>
        index === 0 ? { ...beat, curator_note: 'Start here during incident response.' } : beat,
      ),
    };
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: notedRun });
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Tell me the story of…'), { target: { value: 'graph store' } });
    fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
    expect(await screen.findByText(/Maintainer note \(non-authoritative\):/)).toBeInTheDocument();
    expect(screen.getByText('Start here during incident response.')).toBeInTheDocument();
  });
});
