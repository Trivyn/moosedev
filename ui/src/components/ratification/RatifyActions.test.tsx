// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { api } from '../../api/client';
import { Proposal } from '../../api/types';
import RatifyActions, { isProposed } from './RatifyActions';

vi.mock('../../api/client', () => ({
  api: {
    listProposals: vi.fn(),
    acceptProposal: vi.fn(),
    rejectProposal: vi.fn(),
  },
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const replacement: Proposal = {
  id: 'rec-1',
  iri: 'https://moosedev.dev/kg/Constraint/rec-1',
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
  evidence: null,
  status: 'proposed',
  timestamp: '2026-11-17T14:05:00Z',
  predecessor_iri: 'https://moosedev.dev/kg/Constraint/original',
  predecessor_title: 'Original bounded reads',
  supersession_reason: 'code-diverged',
  claim_diff: '- old claim\n+ new claim',
  diff_truncated: false,
};

describe('RatifyActions', () => {
  it('recognises proposed status in raw and rendered form only', () => {
    expect(isProposed('proposed')).toBe(true);
    expect(isProposed('Proposed')).toBe(true);
    expect(isProposed('Accepted')).toBe(false);
    expect(isProposed(null)).toBe(false);
  });

  it('renders nothing for a record that is not proposed', () => {
    const { container } = render(<RatifyActions uuid="rec-1" status="Accepted" onRatified={vi.fn()} />);
    expect(container).toBeEmptyDOMElement();
    expect(api.listProposals).not.toHaveBeenCalled();
  });

  it('accepts by the record uuid and tells the view to reload', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ proposals: [] });
    vi.mocked(api.acceptProposal).mockResolvedValue({
      id: 'rec-1',
      status: 'accepted',
      entity_iri: null,
      entity_name: null,
    });
    const onRatified = vi.fn();
    render(<RatifyActions uuid="rec-1" status="Proposed" onRatified={onRatified} />);

    fireEvent.click(screen.getByRole('button', { name: 'Accept' }));
    await waitFor(() => expect(onRatified).toHaveBeenCalledTimes(1));
    expect(api.acceptProposal).toHaveBeenCalledWith('rec-1');
    expect(api.rejectProposal).not.toHaveBeenCalled();
  });

  it('shows a failed reject and does not reload', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ proposals: [] });
    vi.mocked(api.rejectProposal).mockRejectedValue(new Error('proposal "rec-1" not found'));
    const onRatified = vi.fn();
    render(<RatifyActions uuid="rec-1" status="proposed" onRatified={onRatified} />);

    fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
    expect(await screen.findByText('proposal "rec-1" not found')).toBeInTheDocument();
    expect(onRatified).not.toHaveBeenCalled();
  });

  it('shows what a replacement supersedes, with the claim diff', async () => {
    vi.mocked(api.listProposals).mockResolvedValue({ proposals: [replacement] });
    const onNavigateRecord = vi.fn();
    render(
      <RatifyActions
        uuid="rec-1"
        status="Proposed"
        onRatified={vi.fn()}
        onNavigateRecord={onNavigateRecord}
      />,
    );

    fireEvent.click(await screen.findByText('Original bounded reads'));
    expect(onNavigateRecord).toHaveBeenCalledWith('https://moosedev.dev/kg/Constraint/original');
    expect(screen.getByText(/reason: code-diverged/)).toBeInTheDocument();
    expect(screen.getByText(/\+ new claim/)).toBeInTheDocument();
  });
});
