import { describe, expect, it, vi } from 'vitest';
import {
  confirmStoryHashNavigation,
  confirmStoryNavigation,
  pageNavigationIsNoop,
  recordRouteForIri,
  recordRouteFromHash,
  recordUuidFromHash,
  storyEntityHash,
  storyEntityUuidFromHash,
  storySubjectNeedsResolving,
} from './routing';

describe('storySubjectNeedsResolving', () => {
  it('does not re-resolve the uuid whose subject is already on screen', () => {
    // The Back trip out of an evidence record re-enters with the same uuid.
    // Re-resolving would tear down the Story kept mounted behind the record.
    expect(storySubjectNeedsResolving('entity-1', 'entity-1', 'urn:iri:one')).toBe(false);
  });

  it('resolves a uuid that has not been resolved yet', () => {
    expect(storySubjectNeedsResolving('entity-1', null, null)).toBe(true);
    // A stale subject from the PREVIOUS deep link must not suppress the new one.
    expect(storySubjectNeedsResolving('entity-2', 'entity-1', 'urn:iri:one')).toBe(true);
    // Same uuid but the lookup failed, so there is nothing to preserve.
    expect(storySubjectNeedsResolving('entity-1', 'entity-1', null)).toBe(true);
  });

  it('has nothing to resolve without a deep link', () => {
    expect(storySubjectNeedsResolving(null, 'entity-1', 'urn:iri:one')).toBe(false);
  });
});

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

describe('storyEntityUuidFromHash', () => {
  it.each([
    ['#/stories/entity/entity-1', 'entity-1'],
    ['#/stories/entity/entity%201', 'entity 1'],
  ])('parses %s', (hash, expected) => {
    expect(storyEntityUuidFromHash(hash)).toBe(expected);
  });

  it.each([
    '#/stories/entity/',
    '#/stories/entity/a/b',
    '#/stories/entity/a%2Fb',
    '#/stories/topic/x',
    '#/stories',
    '#/record/entity-1',
    '#/stories/entity/%',
  ])('rejects %s', (hash) => {
    expect(storyEntityUuidFromHash(hash)).toBeNull();
  });

  it('round-trips a code entity IRI through the canonical Story hash', () => {
    const hash = storyEntityHash('https://moosedev.dev/kg/CodeEntity/entity-1');
    expect(hash).toBe('#/stories/entity/entity-1');
    expect(storyEntityUuidFromHash(hash!)).toBe('entity-1');
  });

  it('has no Story hash for an IRI with no addressable segment', () => {
    expect(storyEntityHash('https://moosedev.dev/kg/CodeEntity/')).toBeNull();
  });

  it('has no Story hash for a fragment-addressed IRI the record route cannot resolve', () => {
    // The daemon matches subjects ending in `/{uuid}`, so a fragment-addressed
    // IRI would advertise a link that resolves to nothing — or to an unrelated
    // subject sharing that suffix. Those keep the in-memory fallback.
    expect(storyEntityHash('https://example.test/records#decision')).toBeNull();
    expect(storyEntityHash('urn:example:decision')).toBeNull();
    // Slash-addressed IRIs still round-trip.
    expect(storyEntityHash('https://moosedev.dev/kg/CodeEntity/entity-1')).toBe(
      '#/stories/entity/entity-1',
    );
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

  it('guards backing out to a hashless origin, which unmounts the editor', () => {
    // The hash is empty here, but the transition still changes pages and
    // unmounts StoriesPage. Treating "no route" as "nothing to confirm" would
    // discard unsaved curation silently.
    const restore = vi.fn();
    const rejectDiscard = vi.fn(() => false);
    const originPage = 'debt';

    expect(confirmStoryHashNavigation(true, originPage, restore, rejectDiscard)).toBe(false);
    expect(restore).toHaveBeenCalledOnce();

    // With nothing to restore, an empty hash really is a non-destination.
    expect(confirmStoryHashNavigation(true, null, restore, rejectDiscard)).toBe(true);
  });

  it('guards a Story deep link the same way it guards a record route', () => {
    const restore = vi.fn();
    const rejectDiscard = vi.fn(() => false);
    const destination = storyEntityUuidFromHash('#/stories/entity/entity-1');

    expect(confirmStoryHashNavigation(true, destination, restore, rejectDiscard)).toBe(false);
    expect(restore).toHaveBeenCalledOnce();

    const acceptDiscard = vi.fn(() => true);
    expect(confirmStoryHashNavigation(true, destination, restore, acceptDiscard)).toBe(true);
    expect(restore).toHaveBeenCalledOnce();
  });
});
