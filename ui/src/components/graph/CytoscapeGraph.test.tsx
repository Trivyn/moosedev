// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { cleanup, render } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import CytoscapeGraph from './CytoscapeGraph';

const mocks = vi.hoisted(() => {
  const handlers: Array<{ event: string; selector?: string; callback: (event: unknown) => void }> = [];
  const core = {
    layout: vi.fn(() => ({ run: vi.fn() })),
    on: vi.fn((event: string, selector: string | ((event: unknown) => void), callback?: (event: unknown) => void) => {
      handlers.push({
        event,
        selector: typeof selector === 'string' ? selector : undefined,
        callback: typeof selector === 'function' ? selector : callback!,
      });
    }),
    destroy: vi.fn(),
    elements: vi.fn(() => ({ removeClass: vi.fn(), style: vi.fn() })),
    fit: vi.fn(),
  };
  const factory = vi.fn((_options: unknown) => core);
  Object.assign(factory, { use: vi.fn() });
  return { core, factory, handlers };
});

vi.mock('cytoscape', () => ({ default: mocks.factory }));
vi.mock('cytoscape-dagre', () => ({ default: {} }));
vi.mock('cytoscape-fcose', () => ({ default: {} }));

const nodes = [
  { id: 'center', label: 'Center', type: 'projectRecord' },
  { id: 'neighbor', label: 'Neighbor', type: 'projectRecord' },
];

afterEach(() => {
  cleanup();
  mocks.handlers.length = 0;
  vi.clearAllMocks();
});

describe('CytoscapeGraph interaction modes', () => {
  it('retains selection and context-menu handlers in the default explore mode', () => {
    render(<CytoscapeGraph nodes={nodes} edges={[]} />);

    expect(mocks.handlers).toEqual(expect.arrayContaining([
      expect.objectContaining({ event: 'tap', selector: 'node, edge' }),
      expect.objectContaining({ event: 'cxttap', selector: 'node' }),
      expect.objectContaining({ event: 'cxttap', selector: 'edge' }),
    ]));
  });

  it('focuses the center and turns node taps into navigation only when opted in', () => {
    const onNodeClick = vi.fn();
    render(
      <CytoscapeGraph
        nodes={nodes}
        edges={[]}
        mode="navigate"
        focusNodeId="center"
        onNodeClick={onNodeClick}
      />,
    );

    const options = mocks.factory.mock.calls.at(-1)?.[0] as { elements: unknown[] } | undefined;
    expect(options?.elements).toEqual(expect.arrayContaining([
      expect.objectContaining({ classes: 'focus-node', data: expect.objectContaining({ id: 'center' }) }),
    ]));
    expect(mocks.handlers).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ event: 'tap', selector: 'node, edge' }),
      expect.objectContaining({ event: 'cxttap' }),
    ]));

    const nodeTap = mocks.handlers.find((handler) => handler.event === 'tap' && handler.selector === 'node');
    nodeTap?.callback({ target: { id: () => 'neighbor' } });
    expect(onNodeClick).toHaveBeenCalledWith(nodes[1]);
  });
});
