import ReactMarkdown, { Components } from 'react-markdown';
import { Link as MuiLink } from '@mui/material';

export type ArtifactKind = 'adrs' | 'requirements' | 'lessons' | 'constraints';

export interface ArtifactTarget {
  kind: ArtifactKind;
  iri: string;
}

interface LinkedMarkdownProps {
  markdown: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
  artifactTargetsByHref?: ReadonlyMap<string, ArtifactTarget>;
}

export function artifactTargetForIri(value: string): ArtifactTarget | null {
  if (value.startsWith('https://moosedev.dev/kg/Requirement/')) {
    return { kind: 'requirements', iri: value };
  }
  if (value.startsWith('https://moosedev.dev/kg/ArchitecturalDecision/')) {
    return { kind: 'adrs', iri: value };
  }
  if (value.startsWith('https://moosedev.dev/kg/Lesson/')) {
    return { kind: 'lessons', iri: value };
  }
  if (value.startsWith('https://moosedev.dev/kg/Constraint/')) {
    return { kind: 'constraints', iri: value };
  }
  return null;
}

function artifactHref(target: ArtifactTarget): string {
  const uuid = target.iri.slice(
    Math.max(target.iri.lastIndexOf('/'), target.iri.lastIndexOf('#')) + 1,
  );
  return `#/${target.kind}/${encodeURIComponent(uuid)}`;
}

export default function LinkedMarkdown({
  markdown,
  onNavigateArtifact,
  artifactTargetsByHref,
}: LinkedMarkdownProps) {
  const components: Components = {
    a({ children, href, node: _node, ...props }) {
      // Generated archives need relative .md links, while the workbench needs
      // those same links routed through the selected artifact's durable UUID.
      const target = href ? artifactTargetsByHref?.get(href) : undefined;
      if (target) {
        return (
          <MuiLink
            href={artifactHref(target)}
            onClick={
              onNavigateArtifact
                ? (event) => {
                    event.preventDefault();
                    onNavigateArtifact(target);
                  }
                : undefined
            }
            {...props}
          >
            {children}
          </MuiLink>
        );
      }
      return (
        <a href={href} {...props}>
          {children}
        </a>
      );
    },
    code({ children, className, ...props }) {
      const value = String(children).replace(/\n$/, '');
      const target = className ? null : artifactTargetForIri(value);
      if (target && onNavigateArtifact) {
        return (
          <MuiLink
            href="#"
            onClick={(event) => {
              event.preventDefault();
              onNavigateArtifact(target);
            }}
            sx={{
              fontFamily: 'monospace',
              overflowWrap: 'anywhere',
              cursor: 'pointer',
            }}
          >
            {value}
          </MuiLink>
        );
      }
      return (
        <code className={className} {...props}>
          {children}
        </code>
      );
    },
  };

  return <ReactMarkdown components={components}>{markdown}</ReactMarkdown>;
}
