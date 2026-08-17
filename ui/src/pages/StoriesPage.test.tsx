// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { api } from '../api/client';
import { StoryRecipe, StoryRun } from '../api/types';
import StoriesPage, { applyAssistedNarration } from './StoriesPage';

vi.mock('../api/client', () => ({
  api: {
    listStories: vi.fn(),
    listStorySubjects: vi.fn(),
    getStory: vi.fn(),
    generateStory: vi.fn(),
    saveStory: vi.fn(),
    publishStory: vi.fn(),
    gradeStoryCheck: vi.fn(),
  },
}));

const componentIri = 'https://moosedev.dev/kg/SystemComponent/graph';
const requirementIri = 'https://moosedev.dev/kg/Requirement/one';
const oldDecisionIri = 'https://moosedev.dev/kg/ArchitecturalDecision/old';
const suppressedIri = 'https://moosedev.dev/kg/ArchitecturalDecision/excluded';

const recipe: StoryRecipe = {
  id: 'graph-store',
  title: 'The graph store',
  schema_version: 3,
  subject: { type: 'entity', iri: componentIri },
  goal: 'Understand the graph boundary.',
  audience: 'reboarding',
  status: 'draft',
  curator: 'James',
  updated_at: '2026-08-13T20:00:00Z',
  focus: {
    include_record_iris: [requirementIri],
    exclude_record_iris: [],
    include_code_symbols: ['scip symbol graph-store'],
    exclude_code_symbols: [],
    emphasis: ['orientation', 'evolution', 'current_state', 'implementation', 'implications'],
  },
  curator_context: 'Explain the transaction boundary to new maintainers.',
};

const run: StoryRun = {
  schema_version: 3,
  recipe_id: null,
  trust_state: 'generated',
  narration_mode: 'symbolic',
  narration_strategy: 'symbolic',
  narration_outcome: 'not_requested',
  title: 'The graph store',
  subject: { type: 'entity', iri: componentIri, kind: 'SystemComponent', label: 'graph/store layer' },
  goal: recipe.goal,
  curator_context: recipe.curator_context,
  brief: { text: 'The graph store keeps authoritative project knowledge queryable.', citation_iris: [requirementIri] },
  narrative: [
    {
      id: 'orientation',
      kind: 'orientation',
      title: 'Why it exists',
      paragraphs: [{ text: 'It externalizes typed knowledge instead of relying on model context.', citation_iris: [requirementIri] }],
    },
    {
      id: 'evolution',
      kind: 'evolution',
      title: 'How it evolved',
      paragraphs: [{ text: 'An earlier decision was superseded when lifecycle history became important.', citation_iris: [oldDecisionIri] }],
    },
    {
      id: 'current-state',
      kind: 'current_state',
      title: 'What is true now',
      paragraphs: [{ text: 'The graph remains authoritative and Story remains read-only.', citation_iris: [requirementIri] }],
    },
  ],
  timeline: [
    {
      id: 'event-old',
      title: 'The earlier graph decision',
      kind: 'ArchitecturalDecision',
      status: 'superseded',
      timestamp: '2025-01-02T03:04:05Z',
      evidence_iri: oldDecisionIri,
      relation: 'Superseded by the current approach',
      predecessor_iris: [],
      successor_iris: [requirementIri],
      rationale_iris: [],
    },
    {
      id: 'event-suppressed',
      title: 'A historical transition',
      kind: 'ArchitecturalDecision',
      status: 'superseded',
      timestamp: '2025-02-02T03:04:05Z',
      evidence_iri: suppressedIri,
      predecessor_iris: [],
      successor_iris: [],
      rationale_iris: [],
    },
  ],
  evidence: [
    {
      iri: requirementIri,
      title: 'Queryable memory',
      kind: 'Requirement',
      status: 'accepted',
      description: 'Project memory must remain external and queryable.',
      timestamp: '2026-08-13T18:00:00Z',
      author: 'James',
      suppressed: false,
      properties: [{ predicate: 'hasPriority', label: 'Priority', value: 'high' }],
      relations: [
        {
          predicate: 'concerns',
          label: 'concerns',
          direction: 'outgoing',
          target_iri: componentIri,
          target_label: 'graph/store layer',
          target_kind: 'SystemComponent',
        },
      ],
    },
    {
      iri: oldDecisionIri,
      title: 'The earlier graph decision',
      kind: 'ArchitecturalDecision',
      status: 'superseded',
      description: 'The original approach omitted lifecycle history.',
      timestamp: '2025-01-02T03:04:05Z',
      suppressed: false,
      properties: [],
      relations: [],
    },
    {
      iri: suppressedIri,
      title: 'Excluded historical detail',
      kind: 'ArchitecturalDecision',
      status: 'superseded',
      suppressed: true,
      properties: [],
      relations: [],
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
  coverage: {
    entity_count: 3,
    current_count: 1,
    historical_count: 2,
    proposed_count: 0,
    code_anchor_count: 1,
    dossier_bytes: 12_288,
    subject_families: ['SystemComponent', 'InformationRecord'],
    outline_sections: ['orientation', 'evolution', 'current_state'],
    truncated: false,
  },
  gaps: [{ id: 'gap-1', title: 'Transaction ownership', detail: 'No accepted rationale is linked.', section_kind: 'implementation' }],
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
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

async function chooseSubjectAndGenerate() {
  const selector = await screen.findByRole('combobox', { name: 'Find an entity' });
  fireEvent.change(selector, { target: { value: 'graph' } });
  fireEvent.click(await screen.findByRole('option', { name: /graph\/store layer/i }));
  fireEvent.click(screen.getByRole('button', { name: 'Tell Story' }));
  await screen.findByRole('heading', { name: 'The graph store', level: 3 });
}

beforeEach(() => {
  vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: run });
  vi.mocked(api.listStories).mockResolvedValue({
    stories: [{
      id: recipe.id,
      title: recipe.title,
      subject: recipe.subject,
      subject_label: 'graph/store layer',
      subject_kind: 'SystemComponent',
      goal: recipe.goal,
      audience: 'reboarding',
      status: 'draft',
      curator: recipe.curator,
    }],
  });
  vi.mocked(api.listStorySubjects).mockResolvedValue({
    subjects: [{ iri: componentIri, kind: 'SystemComponent', label: 'graph/store layer', description: 'Owns src/graph/' }],
  });
  vi.mocked(api.getStory).mockResolvedValue({ recipe });
  vi.mocked(api.saveStory).mockImplementation(async (value) => ({ recipe: { ...value, updated_at: '2026-08-13T20:01:00Z' } }));
  vi.mocked(api.publishStory).mockResolvedValue({ recipe: { ...recipe, status: 'published', updated_at: '2026-08-13T20:02:00Z' } });
  vi.mocked(api.gradeStoryCheck).mockResolvedValue({ correct: true, feedback: 'Correct.', evidence_iris: [requirementIri] });
  Element.prototype.scrollIntoView = vi.fn();
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('Story v3 workbench', () => {
  it('loads the complete categorized entity catalog and generates symbolically first', async () => {
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    await chooseSubjectAndGenerate();

    expect(api.listStorySubjects).toHaveBeenCalledWith(undefined, 5_000);
    expect(api.generateStory).toHaveBeenNthCalledWith(1, { subject_iri: componentIri, assist_level: 0 });
    expect(api.generateStory).toHaveBeenNthCalledWith(2, { subject_iri: componentIri, assist_level: 1, include_checks: false });
  });

  it('renders one article with citations, chronology, code, separate gaps, and an evidence appendix', async () => {
    const navigate = vi.fn();
    render(<StoriesPage onNavigateRecord={navigate} />);
    await chooseSubjectAndGenerate();

    const article = screen.getByRole('article');
    expect(within(article).getByText(run.brief.text)).toBeInTheDocument();
    expect(within(article).getByRole('heading', { name: 'Why it exists' })).toBeInTheDocument();
    expect(within(article).getByRole('heading', { name: 'How it evolved' })).toBeInTheDocument();
    expect(within(article).getByRole('heading', { name: 'Evolution over time' })).toBeInTheDocument();
    expect(within(article).getByText(/Maintainer context \(non-authoritative\)/)).toBeInTheDocument();
    expect(within(article).getByText('src/graph/store.rs:0', { exact: false })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Knowledge gaps' })).not.toBe(article);

    fireEvent.click(within(article).getAllByRole('button', { name: /Evidence 1: Queryable memory/ })[0]);
    expect(navigate).toHaveBeenCalledWith(requirementIri);
    fireEvent.click(within(article).getByRole('button', { name: 'The earlier graph decision' }));
    expect(navigate).toHaveBeenCalledWith(oldDecisionIri);
    fireEvent.click(within(article).getByRole('button', { name: 'A historical transition' }));
    expect(navigate).toHaveBeenCalledWith(suppressedIri);

    fireEvent.click(within(article).getByRole('button', { name: /Evidence appendix/ }));
    expect(within(article).getByText('Project memory must remain external and queryable.')).toBeInTheDocument();
    expect(within(article).getByText('Priority')).toBeInTheDocument();
    expect(within(article).getByText('high')).toBeInTheDocument();
    expect(within(article).getByText('12.0 KiB dossier')).toBeInTheDocument();
    expect(within(article).getByText('InformationRecord')).toBeInTheDocument();
    expect(within(article).getAllByText('Current state').length).toBeGreaterThan(1);
    expect(within(article).queryByText('Excluded historical detail')).not.toBeInTheDocument();
    expect(within(article).getAllByText(/superseded/i).length).toBeGreaterThan(0);
  });

  it('accepts assisted paragraph regrouping while rejecting deterministic projection drift', () => {
    const assisted: StoryRun = {
      ...run,
      narration_mode: 'llm',
      narration_strategy: 'single_pass',
      narration_outcome: 'succeeded',
      brief: { text: 'A clearer opening.', citation_iris: [oldDecisionIri, requirementIri] },
      narrative: run.narrative.map((section, index) => ({
        ...section,
        paragraphs: index === 0
          ? [
              { text: 'A clearer first paragraph.', citation_iris: [oldDecisionIri] },
              { text: 'A new connecting paragraph.', citation_iris: [requirementIri] },
            ]
          : [{ text: `Clear: ${section.paragraphs[0].text}`, citation_iris: [...section.paragraphs[0].citation_iris].reverse() }],
      })),
    };
    assisted.checks = [];
    const merged = applyAssistedNarration(run, assisted);
    expect(merged?.brief).toEqual(assisted.brief);
    expect(merged?.narrative).toEqual(assisted.narrative);
    expect(merged?.narrative[0].paragraphs).toHaveLength(2);
    expect(merged?.checks).toEqual(run.checks);
    expect(applyAssistedNarration(run, { ...assisted, timeline: [] })).toBeNull();
    expect(applyAssistedNarration(run, { ...assisted, evidence: [] })).toBeNull();
    expect(applyAssistedNarration(run, { ...assisted, coverage: { ...assisted.coverage, dossier_bytes: 1 } })).toBeNull();
  });

  it('preserves graded answers while background narration replaces prose', async () => {
    const assisted = deferred<{ outcome: 'story'; story: StoryRun }>();
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockReturnValueOnce(assisted.promise);
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    await chooseSubjectAndGenerate();

    fireEvent.click(screen.getByLabelText('The project knowledge graph'));
    await waitFor(() => expect(screen.getByLabelText('The project knowledge graph')).toBeChecked());
    fireEvent.click(screen.getByRole('button', { name: 'Check answer' }));
    expect(await screen.findByText('Correct.')).toBeInTheDocument();

    await act(async () => assisted.resolve({
      outcome: 'story',
      story: {
        ...run,
        narration_mode: 'llm',
        narration_strategy: 'single_pass',
        narration_outcome: 'succeeded',
        narration_coverage: {
          eligible_entities: 46,
          included_entities: 18,
          source_groups: 9,
          truncated: true,
        },
        brief: { ...run.brief, text: 'A clearer opening.' },
        narrative: run.narrative.map((section) => ({ ...section, paragraphs: section.paragraphs.map((paragraph) => ({ ...paragraph, text: `Clear: ${paragraph.text}` })) })),
      },
    }));
    expect(await screen.findByText('A clearer opening.')).toBeInTheDocument();
    expect(screen.getByText(/It used 18 of 46 eligible evidence entities/)).toBeInTheDocument();
    expect(screen.getByText('Correct.')).toBeInTheDocument();
    expect(screen.getByLabelText('The project knowledge graph')).toBeChecked();
  });

  it('explains categorized narration validation failures without hiding the Story', async () => {
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockResolvedValueOnce({
        outcome: 'story',
        story: {
          ...run,
          narration_outcome: 'invalid_response',
          narration_failure_reason: 'citation_mismatch',
          narration_coverage: {
            eligible_entities: 2,
            included_entities: 2,
            source_groups: 2,
            truncated: false,
          },
        },
      });
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    await chooseSubjectAndGenerate();
    expect(await screen.findByText(/did not preserve the required evidence citations/)).toBeInTheDocument();
    expect(screen.getByText(run.brief.text)).toBeInTheDocument();
  });

  it('scrolls an incorrect answer back to the server-selected narrative section', async () => {
    vi.mocked(api.gradeStoryCheck).mockResolvedValue({
      correct: false,
      feedback: 'Revisit the current state.',
      revisit_section_id: 'current-state',
      evidence_iris: [requirementIri],
    });
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    await chooseSubjectAndGenerate();
    fireEvent.click(screen.getByLabelText('The Story recipe'));
    fireEvent.click(screen.getByRole('button', { name: 'Check answer' }));
    await screen.findByText('Revisit the current state.');
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
  });

  it('saves a generated Story as a v3 focus recipe and guards double clicks', async () => {
    const save = deferred<{ recipe: StoryRecipe }>();
    const expandedRun: StoryRun = {
      ...run,
      evidence: [
        ...run.evidence,
        { iri: componentIri, title: 'graph/store layer', kind: 'SystemComponent', status: 'accepted', suppressed: false, properties: [], relations: [] },
        { iri: 'https://moosedev.dev/kg/Rationale/one', title: 'Why the graph changed', kind: 'Rationale', status: 'accepted', suppressed: false, properties: [], relations: [] },
      ],
      coverage: { ...run.coverage, entity_count: 4, current_count: 3 },
    };
    vi.mocked(api.generateStory).mockResolvedValue({ outcome: 'story', story: expandedRun });
    vi.mocked(api.saveStory).mockReturnValueOnce(save.promise);
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    await chooseSubjectAndGenerate();

    const button = screen.getByRole('button', { name: 'Save as draft' });
    fireEvent.click(button);
    fireEvent.click(button);
    expect(api.saveStory).toHaveBeenCalledTimes(1);
    const saved = vi.mocked(api.saveStory).mock.calls[0][0];
    expect(saved.schema_version).toBe(3);
    expect(saved.focus.include_record_iris).toEqual([]);
    expect(saved.focus.exclude_record_iris).toEqual([]);
    expect(saved.focus.include_code_symbols).toEqual([]);
    expect(saved.focus.exclude_code_symbols).toEqual([]);
    expect(saved.focus.emphasis).toEqual(['orientation', 'evolution', 'current_state']);

    await act(async () => save.resolve({ recipe: { ...saved, updated_at: 'token' } }));
    await waitFor(() => expect(api.generateStory).toHaveBeenCalledWith({ recipe_id: saved.id, assist_level: 0 }));
    await waitFor(() => expect(api.generateStory).toHaveBeenCalledWith({ recipe_id: saved.id, assist_level: 1, include_checks: false }));
  });

  it('does not regenerate when told about the subject already on screen', async () => {
    // The workbench syncs the URL to whatever is displayed, which feeds the
    // same subject straight back in. Acting on it would discard the Story just
    // generated — and with it the reader's progress and graded answers.
    const { rerender } = render(<StoriesPage onNavigateRecord={vi.fn()} />);
    await chooseSubjectAndGenerate();
    const callsAfterGenerate = vi.mocked(api.generateStory).mock.calls.length;

    rerender(<StoriesPage onNavigateRecord={vi.fn()} initialSubjectIri={componentIri} />);

    await waitFor(() => expect(screen.getByRole('article')).toBeInTheDocument());
    expect(api.generateStory).toHaveBeenCalledTimes(callsAfterGenerate);
  });

  it('discards a save that lands after a deep link replaced the Story', async () => {
    const linkedIri = 'https://moosedev.dev/kg/CodeEntity/build-routes';
    const save = deferred<{ recipe: StoryRecipe }>();
    vi.mocked(api.saveStory).mockReturnValueOnce(save.promise);
    const { rerender } = render(<StoriesPage onNavigateRecord={vi.fn()} />);
    await chooseSubjectAndGenerate();

    fireEvent.click(screen.getByRole('button', { name: 'Save as draft' }));
    await waitFor(() => expect(api.saveStory).toHaveBeenCalledTimes(1));
    const saved = vi.mocked(api.saveStory).mock.calls[0][0];

    // The deep link arrives while the save is still in flight. It supersedes
    // what is on screen AND what is in flight.
    rerender(<StoriesPage onNavigateRecord={vi.fn()} initialSubjectIri={linkedIri} />);
    await waitFor(() =>
      expect(api.generateStory).toHaveBeenCalledWith({ subject_iri: linkedIri, assist_level: 0 }),
    );

    await act(async () => save.resolve({ recipe: { ...saved, updated_at: 'token' } }));

    // Reloading the reader here would put the SAVED Story back on screen under
    // the deep-linked URL, so the stale save must abandon its result entirely.
    expect(api.generateStory).not.toHaveBeenCalledWith({
      recipe_id: saved.id,
      assist_level: 0,
    });
  });

  it('curates focus, emphasis, and context instead of editing narrative blocks', async () => {
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    await screen.findByRole('heading', { name: 'Curate Story' });

    fireEvent.change(screen.getByLabelText('Exclude records'), { target: { value: oldDecisionIri } });
    fireEvent.change(screen.getByLabelText('Curator context'), { target: { value: 'Lead with why the history changed.' } });
    fireEvent.click(screen.getByRole('button', { name: 'Move Evolution up' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save draft' }));

    await waitFor(() => expect(api.saveStory).toHaveBeenCalled());
    const saved = vi.mocked(api.saveStory).mock.calls[0][0];
    expect(saved.focus.exclude_record_iris).toEqual([oldDecisionIri]);
    expect(saved.focus.emphasis.slice(0, 2)).toEqual(['evolution', 'orientation']);
    expect(saved.curator_context).toBe('Lead with why the history changed.');
    expect(screen.queryByText(/beat/i)).not.toBeInTheDocument();
  });

  it('names the reloaded Story in the URL after curating straight from the library', async () => {
    // This flow never goes through a Story hash, so without syncing the subject
    // the URL keeps naming nothing while a Story is on screen — and a refresh
    // lands back on the default page instead of the Story being read.
    const onSubjectChange = vi.fn();
    render(<StoriesPage onNavigateRecord={vi.fn()} onSubjectChange={onSubjectChange} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    await screen.findByRole('heading', { name: 'Curate Story' });

    fireEvent.click(screen.getByRole('button', { name: 'Save draft' }));

    await waitFor(() => expect(onSubjectChange).toHaveBeenCalledWith(componentIri));
  });

  it('blocks invalid overlapping focus and preserves a published status on save', async () => {
    vi.mocked(api.getStory).mockResolvedValue({ recipe: { ...recipe, status: 'published' } });
    render(<StoriesPage onNavigateRecord={vi.fn()} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Curate' }));
    await screen.findByRole('heading', { name: 'Curate Story' });

    fireEvent.change(screen.getByLabelText('Exclude records'), { target: { value: requirementIri } });
    expect(screen.getByText(/both included and excluded/i)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save changes' })).toBeDisabled();

    fireEvent.change(screen.getByLabelText('Exclude records'), { target: { value: '' } });
    fireEvent.change(screen.getByLabelText('Learning goal'), { target: { value: 'Updated goal' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(api.saveStory).toHaveBeenCalled());
    expect(vi.mocked(api.saveStory).mock.calls[0][0].status).toBe('published');
  });
});
