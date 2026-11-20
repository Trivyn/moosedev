// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { api } from '../api/client';
import { Proposal } from '../api/types';
import RatificationsPage from './RatificationsPage';

vi.mock('../api/client', () => ({
  api: {
    listProposals: vi.fn(),
    acceptProposal: vi.fn(),
    rejectProposal: vi.fn(),
    recategorizeProposal: vi.fn(),
  },
}));

beforeEach(() => {
  const storedValues = new Map<string, string>();
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => storedValues.get(key) ?? null,
    setItem: (key: string, value: string) => storedValues.set(key, value),
    removeItem: (key: string) => storedValues.delete(key),
    clear: () => storedValues.clear(),
    key: (index: number) => [...storedValues.keys()][index] ?? null,
    get length() {
      return storedValues.size;
    },
  });
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

/** A pending record proposal; tests override only what they assert on. */
function record(id: string, label: string, timestamp: string | null, extra: Partial<Proposal> = {}): Proposal {
  return {
    id,
    iri: `https://moosedev.dev/kg/Lesson/${id}`,
    kind: 'record',
    label,
    subject_iri: '',
    predicate: '',
    target_symbol: '',
    target_path: '',
    record_class: 'Lesson',
    target_iri: '',
    confidence: null,
    escalation: null,
    subject_name: '',
    subject_description: null,
    subject_path: '',
    target_display: '',
    evidence: null,
    status: 'proposed',
    timestamp,
    predecessor_iri: null,
    predecessor_title: null,
    supersession_reason: null,
    claim_diff: null,
    diff_truncated: false,
    ...extra,
  };
}

/** A pending link from a record to a code entity. */
function link(id: string, subjectIri: string, subjectName: string, timestamp: string): Proposal {
  return {
    ...record(id, `${subjectName} concerns ${id}`, timestamp),
    iri: `https://moosedev.dev/kg/ProposedLink/${id}`,
    kind: 'link',
    record_class: null,
    subject_iri: subjectIri,
    subject_name: subjectName,
    predicate: 'concerns',
    target_display: `src/${id}.rs`,
    target_symbol: id,
    target_path: `src/${id}.rs`,
  };
}

/** Card titles in on-screen order (record labels and link-group subject names). */
function cardTitles(): string[] {
  return screen
    .getAllByText(/^(Record|“)/, { selector: 'h6, span' })
    .map((el) => el.textContent ?? '');
}

const INBOX = [
  record('old', 'Record old', '2026-11-01T09:00:00Z'),
  record('undated', 'Record undated', null),
  link('linkmid', 'https://moosedev.dev/kg/ArchitecturalDecision/ad1', 'Link group mid', '2026-11-10T09:00:00Z'),
  record('new', 'Record new', '2026-11-18T09:00:00Z', { evidence: 'Mentions the parser.' }),
];

async function chooseSort(label: string) {
  fireEvent.mouseDown(await screen.findByRole('combobox', { name: 'Sort' }));
  fireEvent.click(within(screen.getByRole('listbox')).getByText(label));
}

describe('RatificationsPage', () => {
  it('shows the predecessor, reason, and bounded claim diff', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({
      proposals: [
        {
          id: 'replacement',
          iri: 'https://moosedev.dev/kg/Constraint/replacement',
          kind: 'record',
          label: 'Bounded source reads',
          subject_iri: '',
          predicate: '',
          target_symbol: '',
          target_path: '',
          record_class: 'Constraint',
          target_iri: '',
          confidence: null,
          escalation: null,
          subject_name: '',
          subject_description: null,
          subject_path: '',
          target_display: '',
          evidence: 'The replacement claim.',
          status: 'proposed',
          timestamp: '2026-11-17T14:05:00Z',
          predecessor_iri: 'https://moosedev.dev/kg/Constraint/original',
          predecessor_title: 'Original bounded reads',
          supersession_reason: 'code-diverged',
          claim_diff: '- Reads never allocate the whole file.\n+ Reads use a bounded buffer.',
          diff_truncated: false,
        },
      ],
    });

    render(<RatificationsPage onNavigateRecord={vi.fn()} />);

    expect(await screen.findByText('Original bounded reads')).toBeInTheDocument();
    expect(screen.getByText(/reason: code-diverged/)).toBeInTheDocument();
    expect(screen.getByText(/Reads never allocate the whole file/)).toBeInTheDocument();
  });

  it('shows when each entry was proposed', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({
      proposals: [
        {
          id: 'pending-lesson',
          iri: 'https://moosedev.dev/kg/Lesson/pending-lesson',
          kind: 'record',
          label: 'Pending lesson',
          subject_iri: '',
          predicate: '',
          target_symbol: '',
          target_path: '',
          record_class: 'Lesson',
          target_iri: '',
          confidence: null,
          escalation: null,
          subject_name: '',
          subject_description: null,
          subject_path: '',
          target_display: '',
          evidence: null,
          status: 'proposed',
          timestamp: '2026-11-17T14:05:00Z',
          predecessor_iri: null,
          predecessor_title: null,
          supersession_reason: null,
          claim_diff: null,
          diff_truncated: false,
        },
      ],
    });

    render(<RatificationsPage onNavigateRecord={vi.fn()} />);

    const expected = new Date('2026-11-17T14:05:00Z').toLocaleString(undefined, {
      dateStyle: 'medium',
      timeStyle: 'short',
    });
    expect(await screen.findByText(expected)).toBeInTheDocument();
  });

  it('keeps the triage order by default: records, then link groups', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ proposals: INBOX });
    render(<RatificationsPage onNavigateRecord={vi.fn()} />);
    await screen.findByText('Record old');
    expect(cardTitles()).toEqual(['Record old', 'Record undated', 'Record new', '“Link group mid”']);
  });

  it('sorts newest or oldest first across records and link groups, undated last', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ proposals: INBOX });
    render(<RatificationsPage onNavigateRecord={vi.fn()} />);

    await chooseSort('Newest first');
    expect(cardTitles()).toEqual(['Record new', '“Link group mid”', 'Record old', 'Record undated']);
    expect(localStorage.getItem('moosedev.ratifications.sort')).toBe('newest');

    await chooseSort('Oldest first');
    expect(cardTitles()).toEqual(['Record old', '“Link group mid”', 'Record new', 'Record undated']);
  });

  it('restores the stored sort on the next visit', async () => {
    localStorage.setItem('moosedev.ratifications.sort', 'newest');
    vi.mocked(api.listProposals).mockResolvedValue({ proposals: INBOX });
    render(<RatificationsPage onNavigateRecord={vi.fn()} />);
    await screen.findByText('Record new');
    expect(cardTitles()[0]).toBe('Record new');
  });

  it('filters on any shown field and says how many match', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ proposals: INBOX });
    render(<RatificationsPage onNavigateRecord={vi.fn()} />);
    const filter = await screen.findByRole('searchbox', { name: 'Filter' });

    fireEvent.change(filter, { target: { value: 'PARSER' } });
    expect(cardTitles()).toEqual(['Record new']);
    expect(screen.getByText('1 of 4 shown')).toBeInTheDocument();

    fireEvent.change(filter, { target: { value: 'src/linkmid' } });
    expect(cardTitles()).toEqual(['“Link group mid”']);

    fireEvent.change(filter, { target: { value: 'nothing like this' } });
    expect(screen.getByText(/Nothing pending matches/)).toBeInTheDocument();
  });
});
