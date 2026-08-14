import { describe, expect, it } from 'vitest';
import { isWorkingSetStatus } from './lifecycle';

describe('isWorkingSetStatus', () => {
  it.each([undefined, null, '', 'accepted', 'Accepted', 'reviewed', ' proposed '])('accepts non-blocklisted status %s', (status) => {
    expect(isWorkingSetStatus(status)).toBe(true);
  });

  it.each(['proposed', 'PROPOSED', 'superseded', 'deprecated', 'rejected'])('rejects non-working status %s', (status) => {
    expect(isWorkingSetStatus(status)).toBe(false);
  });
});
