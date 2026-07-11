// @vitest-environment jsdom
import '@testing-library/jest-dom/vitest';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { api } from '../api/client';
import { ChatResponse } from '../api/types';
import ChatPage from './ChatPage';

vi.mock('../api/client', () => ({
  api: {
    chat: vi.fn(),
    listSessions: vi.fn(),
    getSession: vi.fn(),
    deleteSession: vi.fn(),
  },
}));

vi.mock('../components/graph/CytoscapeGraph', () => ({
  default: () => <div>Graph</div>,
}));

const chatResponse = {
  id: 'chat-1',
  object: 'chat.completion',
  created: 0,
  model: 'test',
  choices: [],
  usage: { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0 },
} satisfies ChatResponse;

const sendMessage = (message = 'What is recorded?') => {
  const input = screen.getByPlaceholderText('Ask about the project knowledge graph');
  fireEvent.change(input, {
    target: { value: message },
  });
  fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
};

const assistButton = (label: string) => screen.getByText(label).closest('button');

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
  vi.mocked(api.listSessions).mockResolvedValue({ sessions: [], count: 0 });
  vi.mocked(api.chat).mockResolvedValue(chatResponse);
});

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe('ChatPage LLM assist level', () => {
  it('sends the default Sensor level', async () => {
    render(<ChatPage />);

    sendMessage();

    await waitFor(() => {
      expect(api.chat).toHaveBeenCalledWith(
        expect.objectContaining({ llm_assist_level: 1 }),
      );
    });
  });

  it('sends and restores the selected Sensor with fallback level', async () => {
    const view = render(<ChatPage />);
    fireEvent.click(assistButton('Sensor + fallback')!);

    sendMessage();

    await waitFor(() => {
      expect(api.chat).toHaveBeenCalledWith(
        expect.objectContaining({ llm_assist_level: 2 }),
      );
      expect(localStorage.getItem('moosedev.chat.assistLevel')).toBe('2');
    });

    view.unmount();
    render(<ChatPage />);

    expect(assistButton('Sensor + fallback')).toHaveAttribute('aria-pressed', 'true');
  });

  it('falls back to Sensor for an invalid stored level', () => {
    localStorage.setItem('moosedev.chat.assistLevel', '7');

    render(<ChatPage />);

    expect(assistButton('Sensor')).toHaveAttribute('aria-pressed', 'true');
  });
});
