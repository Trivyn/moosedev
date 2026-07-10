// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { recordRouteFromHash, recordUuidFromHash } from './App';

describe('recordRouteFromHash', () => {
  it.each([
    ['#/record/abc', { kind: 'record', uuid: 'abc' }],
    ['#/adrs/adr-1', { kind: 'adrs', uuid: 'adr-1' }],
    ['#/requirements/req-1', { kind: 'requirements', uuid: 'req-1' }],
    ['#/lessons/lesson%201', { kind: 'lessons', uuid: 'lesson 1' }],
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
    expect(recordUuidFromHash('#/record/')).toBeNull();
    expect(recordUuidFromHash('#/record/abc/extra')).toBeNull();
  });
});
