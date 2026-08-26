// @vitest-environment jsdom
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../api/client';
import { StoryGenerateResponse, StoryRecipe, StoryRun } from '../../api/types';
import { useStoryGeneration } from './useStoryGeneration';

vi.mock('../../api/client', () => ({
  api: {
    generateStory: vi.fn(),
    getStory: vi.fn(),
    publishStory: vi.fn(),
    saveStory: vi.fn(),
  },
}));

const subjectIri = 'https://moosedev.dev/kg/SystemComponent/graph';

function story(iri = subjectIri, brief = 'Symbolic account.'): StoryRun {
  return {
    schema_version: 3,
    recipe_id: null,
    trust_state: 'generated',
    narration_mode: brief === 'Symbolic account.' ? 'symbolic' : 'llm',
    narration_strategy: brief === 'Symbolic account.' ? 'symbolic' : 'single_pass',
    narration_outcome: brief === 'Symbolic account.' ? 'not_requested' : 'succeeded',
    title: 'The graph store',
    subject: { type: 'entity', iri, kind: 'SystemComponent', label: 'graph/store layer' },
    goal: 'Understand the graph boundary.',
    brief: { text: brief, citation_iris: [] },
    narrative: [],
    timeline: [],
    evidence: [],
    code_anchors: [],
    coverage: {
      entity_count: 0,
      current_count: 0,
      historical_count: 0,
      proposed_count: 0,
      code_anchor_count: 0,
      dossier_bytes: 0,
      subject_families: [],
      outline_sections: [],
      truncated: false,
    },
    gaps: [],
    checks: [],
  };
}

function recipe(run: StoryRun): StoryRecipe {
  return {
    id: 'graph-store',
    title: run.title,
    schema_version: 3,
    subject: { type: 'entity', iri: subjectIri },
    goal: run.goal,
    audience: 'reboarding',
    focus: {
      include_record_iris: [],
      exclude_record_iris: [],
      include_code_symbols: [],
      exclude_code_symbols: [],
      emphasis: [],
    },
    status: 'draft',
    curator: 'maintainer',
    updated_at: 'token',
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, reject, resolve };
}

function renderGeneration(
  refreshLibrary = vi.fn().mockResolvedValue(undefined),
  assistLevel: 0 | 1 = 0,
) {
  const onSubjectChange = vi.fn();
  const hook = renderHook(() => useStoryGeneration({
    assistLevel,
    onSubjectChange,
    refreshLibrary,
  }));
  return { ...hook, onSubjectChange, refreshLibrary };
}

beforeEach(() => {
  vi.mocked(api.generateStory).mockReset();
  vi.mocked(api.getStory).mockReset();
  vi.mocked(api.publishStory).mockReset();
  vi.mocked(api.saveStory).mockReset();
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('useStoryGeneration', () => {
  it('disowns stale work and immediately releases the page during navigation', async () => {
    const stale = deferred<StoryGenerateResponse>();
    vi.mocked(api.generateStory).mockReturnValueOnce(stale.promise);
    const { result } = renderGeneration();

    let generation!: Promise<void>;
    act(() => { generation = result.current.generate({ subject_iri: subjectIri }); });
    await waitFor(() => expect(result.current.busy).toBe(true));

    act(() => result.current.resetForNavigation());
    expect(result.current.busy).toBe(false);
    expect(result.current.generated).toBeNull();

    await act(async () => {
      stale.resolve({ outcome: 'story', story: story() });
      await generation;
    });
    expect(result.current.generated).toBeNull();
    expect(result.current.busy).toBe(false);
  });

  it('leaves no outgoing Story or permanent busy state when a replacement fails', async () => {
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: story() })
      .mockRejectedValueOnce(new Error('subject lookup failed'));
    const { result } = renderGeneration();

    await act(async () => result.current.generate({ subject_iri: subjectIri }));
    expect(result.current.currentStory).not.toBeNull();

    act(() => result.current.replaceWith('https://moosedev.dev/kg/CodeEntity/missing'));
    await waitFor(() => expect(result.current.error).toBe('subject lookup failed'));
    expect(result.current.currentStory).toBeNull();
    expect(result.current.busy).toBe(false);
  });

  it('lets only the current assist own the narration indicator', async () => {
    const firstAssist = deferred<StoryGenerateResponse>();
    const secondAssist = deferred<StoryGenerateResponse>();
    const secondIri = 'https://moosedev.dev/kg/SystemComponent/http';
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: story() })
      .mockReturnValueOnce(firstAssist.promise)
      .mockResolvedValueOnce({ outcome: 'story', story: story(secondIri) })
      .mockReturnValueOnce(secondAssist.promise);
    const { result } = renderGeneration(undefined, 1);

    let firstGeneration!: Promise<void>;
    act(() => { firstGeneration = result.current.generate({ subject_iri: subjectIri }); });
    await waitFor(() => expect(result.current.assisting).toBe(true));

    act(() => result.current.replaceWith(secondIri));
    await waitFor(() => expect(api.generateStory).toHaveBeenCalledTimes(4));
    expect(result.current.assisting).toBe(true);

    await act(async () => {
      firstAssist.resolve({ outcome: 'story', story: story(subjectIri, 'Stale narration.') });
      await firstGeneration;
    });
    expect(result.current.assisting).toBe(true);
    expect(result.current.currentStory?.subject).toMatchObject({ iri: secondIri });

    await act(async () => {
      secondAssist.resolve({ outcome: 'story', story: story(secondIri, 'Current narration.') });
    });
    await waitFor(() => expect(result.current.assisting).toBe(false));
    expect(result.current.currentStory?.brief.text).toBe('Current narration.');
  });

  it('guards duplicate saves and preserves partial-success warnings', async () => {
    const run = story();
    const saved = recipe(run);
    const save = deferred<{ recipe: StoryRecipe }>();
    const refreshLibrary = vi.fn()
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error('library unavailable'));
    vi.mocked(api.generateStory)
      .mockResolvedValueOnce({ outcome: 'story', story: run })
      .mockRejectedValueOnce(new Error('reader unavailable'));
    vi.mocked(api.saveStory).mockReturnValueOnce(save.promise);
    const { result } = renderGeneration(refreshLibrary);

    await act(async () => result.current.generate({ subject_iri: subjectIri }));
    let firstSave!: Promise<void>;
    act(() => {
      firstSave = result.current.saveGenerated();
      void result.current.saveGenerated();
    });
    expect(api.saveStory).toHaveBeenCalledTimes(1);

    await act(async () => {
      save.resolve({ recipe: saved });
      await firstSave;
    });
    expect(result.current.warning).toBe(
      'Story was saved as draft, but its reader could not be reloaded: reader unavailable '
      + 'Story was saved as draft, but the library could not be refreshed: library unavailable',
    );
    expect(result.current.busy).toBe(false);
  });
});
