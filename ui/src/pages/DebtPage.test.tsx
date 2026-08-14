// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { api } from '../api/client';
import DebtPage from './DebtPage';

vi.mock('../api/client', () => ({
  api: { debt: vi.fn() },
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('DebtPage', () => {
  it('launches a Story for a component without navigating the record row', async () => {
    vi.mocked(api.debt).mockResolvedValue({
      unmapped: 0,
      components: [
        {
          iri: 'https://moosedev.dev/kg/SystemComponent/graph',
          story_component_iri: 'https://moosedev.dev/kg/SystemComponent/graph',
          name: 'graph/store layer',
          numerator: 2,
          denominator: 4,
          coverage: 0.5,
          core_numerator: 1,
          core_denominator: 2,
          undocumented: ['GraphStore'],
        },
      ],
    });
    const navigateRecord = vi.fn();
    const tellStory = vi.fn();
    render(<DebtPage onNavigateRecord={navigateRecord} onTellStory={tellStory} />);

    fireEvent.click(await screen.findByRole('button', { name: 'Tell the Story of graph/store layer' }));

    expect(tellStory).toHaveBeenCalledWith('https://moosedev.dev/kg/SystemComponent/graph');
    expect(navigateRecord).not.toHaveBeenCalled();
  });

  it('suppresses Story launch for a non-working-set component', async () => {
    vi.mocked(api.debt).mockResolvedValue({
      unmapped: 0,
      components: [
        {
          iri: 'https://moosedev.dev/kg/SystemComponent/old',
          name: 'old graph layer',
          story_component_iri: null,
          numerator: 0,
          denominator: 1,
          coverage: 0,
          core_numerator: 0,
          core_denominator: 0,
          undocumented: ['OldStore'],
        },
      ],
    });

    render(<DebtPage onNavigateRecord={vi.fn()} onTellStory={vi.fn()} />);
    await screen.findByText('old graph layer');

    expect(screen.queryByRole('button', { name: 'Tell the Story of old graph layer' })).not.toBeInTheDocument();
  });
});
