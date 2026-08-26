// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { api } from '../api/client';
import RatificationsPage from './RatificationsPage';

vi.mock('../api/client', () => ({
  api: {
    listProposals: vi.fn(),
    acceptProposal: vi.fn(),
    rejectProposal: vi.fn(),
    recategorizeProposal: vi.fn(),
  },
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

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
});
