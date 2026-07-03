// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import ClarificationCard from './ClarificationCard';
import { ClarificationReply, ClarificationRequest } from '../../api/types';

afterEach(cleanup);

function makeRequest(overrides: Partial<ClarificationRequest> = {}): ClarificationRequest {
  return {
    id: 'req-1',
    session_id: 'sess-1',
    turn_number: 1,
    question: 'Did you mean the Constraint class?',
    original_question: 'list constraints',
    slot_kind: { kind: 'UnknownTerm', data: { noun: 'constraints' } },
    expected_kinds: ['Class'],
    candidates: [
      {
        iri: 'https://example.org/Constraint',
        local_name: 'Constraint',
        label: 'Constraint',
        kind: 'Class',
        score: 0.9,
      },
      { iri: 'https://example.org/Requirement', local_name: 'Requirement', kind: 'Class', score: 0.4 },
    ],
    trigger: 'AmbiguousMatch',
    created_at: '2026-07-01T00:00:00Z',
    unresolved_surface: 'constraints',
    ...overrides,
  };
}

describe('ClarificationCard', () => {
  it('renders the question and one chip per candidate', () => {
    render(<ClarificationCard request={makeRequest()} onReply={() => {}} />);
    expect(screen.getByText('Did you mean the Constraint class?')).toBeTruthy();
    expect(screen.getByText('Constraint')).toBeTruthy();
    expect(screen.getByText('Requirement')).toBeTruthy();
  });

  it('submits a remembered PickCandidate reply when a chip is clicked', () => {
    const onReply = vi.fn();
    render(<ClarificationCard request={makeRequest()} onReply={onReply} />);
    fireEvent.click(screen.getByText('Constraint'));

    expect(onReply).toHaveBeenCalledTimes(1);
    const reply = onReply.mock.calls[0][0] as ClarificationReply;
    expect(reply.id).toBe('req-1');
    expect(reply.action).toEqual({
      kind: 'PickCandidate',
      data: { iri: 'https://example.org/Constraint' },
    });
    // A teachable slot (unresolved_surface set) is remembered to the user overlay.
    expect(reply.remember_for_user).toBe(true);
    expect(reply.agent).toEqual({ kind: 'Human', data: { user_id: null } });
  });

  it('does not remember a pick when the slot carries no persistable surface', () => {
    const onReply = vi.fn();
    const request = makeRequest({ slot_kind: { kind: 'PickCandidate' }, unresolved_surface: null });
    render(<ClarificationCard request={request} onReply={onReply} />);
    fireEvent.click(screen.getByText('Constraint'));
    expect((onReply.mock.calls[0][0] as ClarificationReply).remember_for_user).toBe(false);
  });

  it('submits a Decline reply', () => {
    const onReply = vi.fn();
    render(<ClarificationCard request={makeRequest()} onReply={onReply} />);
    fireEvent.click(screen.getByText('Decline'));
    const reply = onReply.mock.calls[0][0] as ClarificationReply;
    expect(reply.action).toEqual({ kind: 'Decline' });
    expect(reply.remember_for_user).toBe(false);
  });

  it('shows an info alert and no chips when there are no candidates', () => {
    render(<ClarificationCard request={makeRequest({ candidates: [] })} onReply={() => {}} />);
    expect(screen.getByText(/No suggestions available/)).toBeTruthy();
    expect(screen.queryByText('Constraint')).toBeNull();
  });

  it('does not fire onReply once disabled (answered)', () => {
    const onReply = vi.fn();
    render(<ClarificationCard request={makeRequest()} onReply={onReply} disabled />);
    fireEvent.click(screen.getByText('Constraint'));
    expect(onReply).not.toHaveBeenCalled();
  });
});
