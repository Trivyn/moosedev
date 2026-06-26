import ReactMarkdown, { Components } from 'react-markdown';
import { Link as MuiLink } from '@mui/material';

export type ArtifactKind = 'adrs' | 'requirements';

export interface ArtifactTarget {
  kind: ArtifactKind;
  iri: string;
}

interface LinkedMarkdownProps {
  markdown: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
}

function artifactTargetForIri(value: string): ArtifactTarget | null {
  if (value.startsWith('https://moosedev.dev/kg/Requirement/')) {
    return { kind: 'requirements', iri: value };
  }
  if (value.startsWith('https://moosedev.dev/kg/ArchitecturalDecision/')) {
    return { kind: 'adrs', iri: value };
  }
  return null;
}

export default function LinkedMarkdown({ markdown, onNavigateArtifact }: LinkedMarkdownProps) {
  const components: Components = {
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
