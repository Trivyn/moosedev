// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import LinkedMarkdown, { artifactTargetForIri } from './LinkedMarkdown';

afterEach(cleanup);

describe('artifactTargetForIri', () => {
  it('maps Constraint IRIs to the typed constraints interface', () => {
    const iri = 'https://moosedev.dev/kg/Constraint/constraint-1';

    expect(artifactTargetForIri(iri)).toEqual({ kind: 'constraints', iri });
  });

  it('does not claim unsupported record kinds', () => {
    expect(artifactTargetForIri('https://moosedev.dev/kg/Pattern/pattern-1')).toBeNull();
  });
});

describe('LinkedMarkdown', () => {
  it('navigates inline Constraint IRIs through the typed artifact callback', () => {
    const iri = 'https://moosedev.dev/kg/Constraint/constraint-1';
    const onNavigateArtifact = vi.fn();

    render(<LinkedMarkdown markdown={`See \`${iri}\`.`} onNavigateArtifact={onNavigateArtifact} />);
    fireEvent.click(screen.getByRole('link', { name: iri }));

    expect(onNavigateArtifact).toHaveBeenCalledWith({ kind: 'constraints', iri });
  });
});
