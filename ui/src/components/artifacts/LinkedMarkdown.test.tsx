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

  it('navigates generated artifact filenames through the typed artifact callback', () => {
    const target = {
      kind: 'adrs' as const,
      iri: 'https://moosedev.dev/kg/ArchitecturalDecision/adr-2',
    };
    const onNavigateArtifact = vi.fn();

    render(
      <LinkedMarkdown
        markdown="Superseded by [ADR-0002](0002-new-decision.md)"
        onNavigateArtifact={onNavigateArtifact}
        artifactTargetsByHref={new Map([['0002-new-decision.md', target]])}
      />,
    );
    const link = screen.getByRole('link', { name: 'ADR-0002' });
    expect(link.getAttribute('href')).toBe('#/adrs/adr-2');
    fireEvent.click(link);

    expect(onNavigateArtifact).toHaveBeenCalledWith(target);
  });

  it('preserves links that do not match a generated artifact filename', () => {
    const onNavigateArtifact = vi.fn();

    render(
      <LinkedMarkdown
        markdown="Read the [missing artifact](missing.md) or [external documentation](https://example.com/docs)."
        onNavigateArtifact={onNavigateArtifact}
        artifactTargetsByHref={new Map()}
      />,
    );

    const missingLink = screen.getByRole('link', { name: 'missing artifact' });
    expect(missingLink.getAttribute('href')).toBe('missing.md');
    const link = screen.getByRole('link', { name: 'external documentation' });
    expect(link.getAttribute('href')).toBe('https://example.com/docs');
    expect(onNavigateArtifact).not.toHaveBeenCalled();
  });
});
