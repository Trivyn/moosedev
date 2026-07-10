import { useEffect, useState } from 'react';
import {
  Alert,
  Box,
  Chip,
  CircularProgress,
  Divider,
  List,
  ListItemButton,
  Stack,
  Typography,
} from '@mui/material';
import { api } from '../api/client';
import { RecordDetailResponse } from '../api/types';
import LinkedMarkdown, { artifactTargetForIri, ArtifactTarget } from '../components/artifacts/LinkedMarkdown';

interface RecordPageProps {
  uuid: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
  onResolveArtifact?: (target: ArtifactTarget) => void;
}

function recordUuidFromIri(iri: string): string | null {
  const uuid = iri.slice(iri.lastIndexOf('/') + 1);
  return uuid || null;
}

function navigateToRecord(iri: string, onNavigateArtifact?: (target: ArtifactTarget) => void) {
  const artifact = artifactTargetForIri(iri);
  if (artifact && onNavigateArtifact) {
    onNavigateArtifact(artifact);
    return;
  }

  const uuid = recordUuidFromIri(iri);
  if (uuid) {
    window.location.hash = `#/record/${encodeURIComponent(uuid)}`;
  }
}

interface RecordEdge {
  predicate: string;
  iri: string;
  label: string;
  kind: string;
}

function EdgeList({
  title,
  edges,
  onNavigateArtifact,
}: {
  title: string;
  edges: RecordEdge[];
  onNavigateArtifact?: (target: ArtifactTarget) => void;
}) {
  return (
    <Box>
      <Typography variant="subtitle1" sx={{ fontWeight: 650, mb: 0.5 }}>
        {title}
      </Typography>
      {edges.length ? (
        <List disablePadding dense>
          {edges.map((edge) => {
            return (
              <ListItemButton
                key={`${edge.predicate}:${edge.iri}`}
                onClick={() => navigateToRecord(edge.iri, onNavigateArtifact)}
                sx={{ borderRadius: 1, alignItems: 'flex-start', px: 1 }}
              >
                <Box sx={{ minWidth: 0 }}>
                  <Typography variant="caption" color="text.secondary">
                    {edge.predicate}
                  </Typography>
                  <Typography variant="body2" sx={{ overflowWrap: 'anywhere' }}>
                    {edge.label}
                    {edge.kind ? ` (${edge.kind})` : ''}
                  </Typography>
                </Box>
              </ListItemButton>
            );
          })}
        </List>
      ) : (
        <Typography variant="body2" color="text.secondary">
          None.
        </Typography>
      )}
    </Box>
  );
}

export default function RecordPage({ uuid, onNavigateArtifact, onResolveArtifact }: RecordPageProps) {
  const [record, setRecord] = useState<RecordDetailResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setRecord(null);
    setError(null);

    api
      .record(uuid)
      .then((response) => {
        if (cancelled) {
          return;
        }
        const artifact = artifactTargetForIri(response.iri);
        const resolveArtifact = onResolveArtifact ?? onNavigateArtifact;
        if (artifact && resolveArtifact) {
          resolveArtifact(artifact);
          return;
        }
        setRecord(response);
      })
      .catch((err) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err));
        }
      });

    return () => {
      cancelled = true;
    };
  }, [uuid]);

  if (error) {
    return (
      <Box sx={{ p: 2 }}>
        <Alert severity="error">{error}</Alert>
      </Box>
    );
  }

  if (!record) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center' }}>
        <CircularProgress size={24} aria-label="Loading record" />
      </Box>
    );
  }

  const metadata = [record.status, record.timestamp, record.author].filter(
    (value): value is string => Boolean(value),
  );
  const outgoing = record.outgoing.map((edge) => ({
    predicate: edge.predicate,
    iri: edge.target_iri,
    label: edge.target_label,
    kind: edge.target_kind,
  }));
  const incoming = record.incoming.map((edge) => ({
    predicate: edge.predicate,
    iri: edge.source_iri,
    label: edge.source_label,
    kind: edge.source_kind,
  }));

  return (
    <Box sx={{ height: '100%', overflow: 'auto', p: { xs: 2, md: 3 }, bgcolor: 'background.default' }}>
      <Stack spacing={2.5} sx={{ maxWidth: 900 }}>
        <Box>
          <Stack direction="row" spacing={1} alignItems="center" useFlexGap flexWrap="wrap">
            <Chip size="small" label={record.kind} />
            <Typography variant="h5">{record.title}</Typography>
          </Stack>
          {metadata.length > 0 && (
            <Typography variant="body2" color="text.secondary" sx={{ mt: 0.75 }}>
              {metadata.join(' · ')}
            </Typography>
          )}
        </Box>

        {record.description && (
          <Box>
            <LinkedMarkdown markdown={record.description} onNavigateArtifact={onNavigateArtifact} />
          </Box>
        )}

        <Divider />
        <EdgeList
          title="Outgoing"
          edges={outgoing}
          onNavigateArtifact={onNavigateArtifact}
        />
        <EdgeList
          title="Incoming"
          edges={incoming}
          onNavigateArtifact={onNavigateArtifact}
        />
      </Stack>
    </Box>
  );
}
