// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import RecordPage from './RecordPage';

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
  it('renders record details and outgoing edges', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue({ ok: true, json: async () => response }),
    );

    render(<RecordPage uuid="record-1" />);

    expect(await screen.findByText('Keep local operation')).toBeInTheDocument();
    expect(screen.getByText('Constraint')).toBeInTheDocument();
    expect(screen.getByText('Outgoing')).toBeInTheDocument();
    expect(screen.getByText(/HTTP server/)).toBeInTheDocument();
    expect(screen.getByText('constrains')).toBeInTheDocument();
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
});
