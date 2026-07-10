import { useEffect, useState } from 'react';
import {
  Alert,
  Box,
  Chip,
  CircularProgress,
  Divider,
  Stack,
  Typography,
} from '@mui/material';
import { api } from '../api/client';
import { RecordDetailResponse } from '../api/types';
import LinkedMarkdown, { artifactTargetForIri, ArtifactTarget } from '../components/artifacts/LinkedMarkdown';
import RecordNeighborhoodGraph from '../components/graph/RecordNeighborhoodGraph';

interface RecordPageProps {
  uuid: string;
  onNavigateArtifact?: (target: ArtifactTarget) => void;
  onNavigateRecord?: (iri: string) => void;
  onResolveArtifact?: (target: ArtifactTarget) => void;
}

export default function RecordPage({
  uuid,
  onNavigateArtifact,
  onNavigateRecord,
  onResolveArtifact,
}: RecordPageProps) {
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
  return (
    <Box sx={{ height: '100%', overflow: 'auto', p: { xs: 2, md: 3 }, bgcolor: 'background.default' }}>
      <Stack spacing={2.5} sx={{ maxWidth: 1100 }}>
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

        <Box>
          <Typography variant="subtitle1" sx={{ fontWeight: 650, mb: 1 }}>
            Connections
          </Typography>
          <RecordNeighborhoodGraph record={record} onNavigateRecord={onNavigateRecord} />
        </Box>

        {record.description && (
          <>
            <Divider />
            <Box sx={{ maxWidth: 900 }}>
              <LinkedMarkdown markdown={record.description} onNavigateArtifact={onNavigateArtifact} />
            </Box>
          </>
        )}
      </Stack>
    </Box>
  );
}
