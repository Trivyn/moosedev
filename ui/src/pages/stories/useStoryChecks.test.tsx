// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { api } from '../../api/client';
import { StoryCheckGradeResponse, StoryRun } from '../../api/types';
import { useStoryChecks } from './useStoryChecks';

vi.mock('../../api/client', () => ({
  api: { gradeStoryCheck: vi.fn() },
}));

const story: StoryRun = {
  schema_version: 3,
  recipe_id: 'quiz-story',
  trust_state: 'generated',
  narration_mode: 'symbolic',
  narration_strategy: 'symbolic',
  narration_outcome: 'not_requested',
  title: 'Quiz Story',
  subject: {
    type: 'entity',
    iri: 'https://moosedev.dev/kg/SystemComponent/quiz',
    kind: 'SystemComponent',
    label: 'Quiz component',
  },
  goal: 'Test understanding.',
  brief: { text: 'A grounded Story.', citation_iris: [] },
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
  checks: [
    {
      id: 'check-a',
      question: 'Question A',
      options: [
        { id: 'a-1', label: 'A one' },
        { id: 'a-2', label: 'A two' },
      ],
    },
    {
      id: 'check-b',
      question: 'Question B',
      options: [
        { id: 'b-1', label: 'B one' },
        { id: 'b-2', label: 'B two' },
      ],
    },
  ],
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function CheckHarness({ value = story }: { value?: StoryRun }) {
  const { selected, results, gradeErrors, grading, selectAnswer, grade } = useStoryChecks(value);
  return (
    <>
      {value.checks.map((check) => (
        <section key={check.id}>
          {check.options.map((option) => (
            <button
              key={option.id}
              type="button"
              onClick={() => selectAnswer(check.id, option.id)}
            >
              Select {option.id}
            </button>
          ))}
          <button
            type="button"
            disabled={!selected[check.id] || grading[check.id]}
            onClick={() => grade(check.id)}
          >
            Grade {check.id}
          </button>
          {results[check.id] ? (
            <output data-testid={`result-${check.id}`}>{results[check.id].feedback}</output>
          ) : null}
          {gradeErrors[check.id] ? (
            <output data-testid={`error-${check.id}`}>{gradeErrors[check.id]}</output>
          ) : null}
        </section>
      ))}
    </>
  );
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('useStoryChecks', () => {
  it('keeps concurrent check results when requests resolve out of order', async () => {
    const first = deferred<StoryCheckGradeResponse>();
    const second = deferred<StoryCheckGradeResponse>();
    vi.mocked(api.gradeStoryCheck).mockImplementation(({ check_id }) =>
      check_id === 'check-a' ? first.promise : second.promise,
    );
    render(<CheckHarness />);

    fireEvent.click(screen.getByRole('button', { name: 'Select a-1' }));
    fireEvent.click(screen.getByRole('button', { name: 'Grade check-a' }));
    fireEvent.click(screen.getByRole('button', { name: 'Select b-1' }));
    fireEvent.click(screen.getByRole('button', { name: 'Grade check-b' }));

    await act(async () => second.resolve({
      correct: true,
      feedback: 'B is correct.',
      evidence_iris: [],
    }));
    expect(await screen.findByTestId('result-check-b')).toHaveTextContent('B is correct.');
    expect(screen.queryByTestId('result-check-a')).not.toBeInTheDocument();

    await act(async () => first.resolve({
      correct: true,
      feedback: 'A is correct.',
      evidence_iris: [],
    }));
    expect(await screen.findByTestId('result-check-a')).toHaveTextContent('A is correct.');
    expect(screen.getByTestId('result-check-b')).toHaveTextContent('B is correct.');
  });

  it('clears feedback and invalidates an in-flight grade when the answer changes', async () => {
    const stale = deferred<StoryCheckGradeResponse>();
    vi.mocked(api.gradeStoryCheck)
      .mockResolvedValueOnce({ correct: true, feedback: 'Earlier feedback.', evidence_iris: [] })
      .mockReturnValueOnce(stale.promise);
    render(<CheckHarness />);

    fireEvent.click(screen.getByRole('button', { name: 'Select a-1' }));
    fireEvent.click(screen.getByRole('button', { name: 'Grade check-a' }));
    expect(await screen.findByTestId('result-check-a')).toHaveTextContent('Earlier feedback.');

    fireEvent.click(screen.getByRole('button', { name: 'Grade check-a' }));
    await waitFor(() => expect(
      screen.getByRole('button', { name: 'Grade check-a' }),
    ).toBeDisabled());
    fireEvent.click(screen.getByRole('button', { name: 'Select a-2' }));

    expect(screen.queryByTestId('result-check-a')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Grade check-a' })).toBeEnabled();

    await act(async () => stale.resolve({
      correct: false,
      feedback: 'Stale feedback.',
      evidence_iris: [],
    }));
    expect(screen.queryByText('Stale feedback.')).not.toBeInTheDocument();
    expect(screen.queryByTestId('result-check-a')).not.toBeInTheDocument();
  });

  it('shows grade failures and re-enables retry', async () => {
    vi.mocked(api.gradeStoryCheck)
      .mockRejectedValueOnce(new Error('grading unavailable'))
      .mockResolvedValueOnce({ correct: true, feedback: 'Retry succeeded.', evidence_iris: [] });
    render(<CheckHarness />);

    fireEvent.click(screen.getByRole('button', { name: 'Select a-1' }));
    fireEvent.click(screen.getByRole('button', { name: 'Grade check-a' }));

    expect(await screen.findByTestId('error-check-a')).toHaveTextContent('grading unavailable');
    const gradeButton = screen.getByRole('button', { name: 'Grade check-a' });
    expect(gradeButton).toBeEnabled();

    fireEvent.click(gradeButton);
    expect(await screen.findByTestId('result-check-a')).toHaveTextContent('Retry succeeded.');
    expect(screen.queryByTestId('error-check-a')).not.toBeInTheDocument();
  });
});
