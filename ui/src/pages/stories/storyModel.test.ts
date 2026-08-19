import { describe, expect, it } from 'vitest';
import { filterStorySubjects } from './storyModel';
import { StorySubjectCandidate } from '../../api/types';

const component: StorySubjectCandidate = {
  iri: 'https://moosedev.dev/kg/SystemComponent/graph',
  kind: 'SystemComponent',
  label: 'graph/store layer',
};
const unrecorded: StorySubjectCandidate = {
  iri: 'https://moosedev.dev/kg/CodeEntity/read-proven',
  kind: 'CodeEntity',
  label: 'read_proven',
  description: 'src/code/substrate/resolver.rs',
  no_recorded_knowledge: true,
};

describe('filterStorySubjects', () => {
  it('browses only the subjects the graph records knowledge about', () => {
    expect(filterStorySubjects([component, unrecorded], '')).toEqual([component]);
    expect(filterStorySubjects([component, unrecorded], '   ')).toEqual([component]);
  });

  it('keeps the reader\'s own selection browsable even when unrecorded', () => {
    expect(filterStorySubjects([component, unrecorded], '', unrecorded)).toEqual([
      component,
      unrecorded,
    ]);
  });

  it('still finds an unrecorded subject by name, so nothing indexed is unreachable', () => {
    expect(filterStorySubjects([component, unrecorded], 'read_prov')).toEqual([unrecorded]);
    expect(filterStorySubjects([component, unrecorded], 'resolver.rs')).toEqual([unrecorded]);
  });
});
