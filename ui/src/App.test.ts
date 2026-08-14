// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';
import {
  confirmStoryHashNavigation,
  confirmStoryNavigation,
  pageNavigationIsNoop,
  recordRouteForIri,
  recordRouteForPage,
  recordRouteFromHash,
  recordUuidFromHash,
} from './App';

describe('pageNavigationIsNoop', () => {
  it('treats the active page as a navigation target when a deep link is open', () => {
    expect(pageNavigationIsNoop('stories', 'stories', null)).toBe(true);
    expect(
      pageNavigationIsNoop('stories', 'stories', {
        kind: 'record',
        uuid: 'evidence-1',
      }),
    ).toBe(false);
    expect(pageNavigationIsNoop('stories', 'debt', null)).toBe(false);
  });
});

describe('recordRouteFromHash', () => {
  it.each([
    ['#/record/abc', { kind: 'record', uuid: 'abc' }],
    ['#/adrs/adr-1', { kind: 'adrs', uuid: 'adr-1' }],
    ['#/requirements/req-1', { kind: 'requirements', uuid: 'req-1' }],
    ['#/lessons/lesson%201', { kind: 'lessons', uuid: 'lesson 1' }],
    ['#/constraints/constraint-1', { kind: 'constraints', uuid: 'constraint-1' }],
  ])('parses %s', (hash, expected) => {
    expect(recordRouteFromHash(hash)).toEqual(expected);
  });

  it.each(['#/adrs/', '#/record/abc/extra', '#/record/a%2Fb', '#/patterns/id', '#/record/%'])(
    'rejects %s',
    (hash) => {
      expect(recordRouteFromHash(hash)).toBeNull();
    },
  );
});

describe('recordUuidFromHash', () => {
  it('returns the record uuid from a record deep link', () => {
    expect(recordUuidFromHash('#/record/abc')).toBe('abc');
  });

  it('returns null for non-record hashes', () => {
    expect(recordUuidFromHash('#/adrs/abc')).toBeNull();
    expect(recordUuidFromHash('#/constraints/abc')).toBeNull();
    expect(recordUuidFromHash('#/record/')).toBeNull();
    expect(recordUuidFromHash('#/record/abc/extra')).toBeNull();
  });
});

describe('recordRouteForIri', () => {
  it.each([
    ['https://moosedev.dev/kg/ArchitecturalDecision/adr-1', { kind: 'adrs', uuid: 'adr-1' }],
    ['https://moosedev.dev/kg/Requirement/req-1', { kind: 'requirements', uuid: 'req-1' }],
    ['https://moosedev.dev/kg/Lesson/lesson-1', { kind: 'lessons', uuid: 'lesson-1' }],
    ['https://moosedev.dev/kg/Constraint/constraint-1', { kind: 'constraints', uuid: 'constraint-1' }],
    ['https://moosedev.dev/kg/CodeEntity/code-1', { kind: 'record', uuid: 'code-1' }],
  ])('maps %s to its canonical route', (iri, expected) => {
    expect(recordRouteForIri(iri)).toEqual(expected);
  });

  it('does not navigate external graph nodes', () => {
    expect(recordRouteForIri('https://example.com/entity/one')).toBeNull();
  });
});

describe('recordRouteForPage', () => {
  it('keeps typed evidence on the generic record route inside Stories', () => {
    const iri = 'https://moosedev.dev/kg/Requirement/req-1';
    expect(recordRouteForPage(iri, 'stories')).toEqual({ kind: 'record', uuid: 'req-1' });
    expect(recordRouteForPage(iri, 'requirements')).toEqual({
      kind: 'requirements',
      uuid: 'req-1',
    });
  });
});

describe('confirmStoryNavigation', () => {
  it('allows clean navigation without prompting and honors the dirty-editor choice', () => {
    const confirmDiscard = vi.fn(() => false);
    expect(confirmStoryNavigation(false, confirmDiscard)).toBe(true);
    expect(confirmDiscard).not.toHaveBeenCalled();
    expect(confirmStoryNavigation(true, confirmDiscard)).toBe(false);
    confirmDiscard.mockReturnValue(true);
    expect(confirmStoryNavigation(true, confirmDiscard)).toBe(true);
  });

  it('restores the accepted location when dirty hash navigation is rejected', () => {
    const restore = vi.fn();
    const rejectDiscard = vi.fn(() => false);
    const route = { kind: 'record' as const, uuid: 'record-1' };

    expect(confirmStoryHashNavigation(true, route, restore, rejectDiscard)).toBe(false);
    expect(restore).toHaveBeenCalledOnce();
    expect(confirmStoryHashNavigation(true, null, restore, rejectDiscard)).toBe(true);
    expect(rejectDiscard).toHaveBeenCalledOnce();
  });
});
