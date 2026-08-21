import { useEffect, useState } from 'react';
import { Alert, Box, Button, Chip, CircularProgress, Stack, Typography } from '@mui/material';
import UnfoldMoreIcon from '@mui/icons-material/UnfoldMore';
import UnfoldLessIcon from '@mui/icons-material/UnfoldLess';
import { api } from '../../api/client';
import { RecordCodeDetail, RecordSourceResponse, SourceScope, SourceSpan } from '../../api/types';

interface CodeEntityPanelProps {
  uuid: string;
  code: RecordCodeDetail;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * The source-aware half of a CodeEntity record page.
 *
 * Everything shown here is a projection of the daemon's current index. The
 * daemon decides whether source can be trusted; this component never falls
 * back to another way of finding the code, it just explains the absence.
 */
export default function CodeEntityPanel({ uuid, code }: CodeEntityPanelProps) {
  const [scope, setScope] = useState<SourceScope>('context');
  const [source, setSource] = useState<RecordSourceResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!code.source_available) {
      setSource(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .recordSource(uuid, scope)
      .then((response) => {
        if (!cancelled) setSource(response);
      })
      .catch((err) => {
        if (!cancelled) {
          // KEEP the last good preview. The scope toggle only renders while a
          // source is loaded, so discarding it on a failed expansion would
          // strand the reader with no way back to the view that worked.
          setError(errorMessage(err));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [uuid, scope, code.source_available]);

  const path = code.source_path ?? code.defined_in_path;
  const definition = source?.definition ?? code.definition;
  const location = path && definition ? `${path}:${definition.start_line}` : path;

  return (
    <Box>
      <Typography variant="subtitle1" sx={{ fontWeight: 650, mb: 1 }}>
        Code
      </Typography>
      <Stack spacing={1.25}>
        <Stack direction="row" spacing={1} alignItems="center" useFlexGap flexWrap="wrap">
          {code.entity_kind ? <Chip size="small" variant="outlined" label={code.entity_kind} /> : null}
          {location ? (
            <Typography variant="body2" sx={{ fontFamily: 'monospace' }}>
              {location}
            </Typography>
          ) : null}
          {code.substrate_stale ? (
            <Chip size="small" color="warning" variant="outlined" label="Index is behind HEAD" />
          ) : null}
        </Stack>

        {code.signature ? (
          <Box
            component="pre"
            sx={{
              m: 0,
              px: 1.25,
              py: 1,
              overflowX: 'auto',
              borderRadius: 1,
              fontFamily: 'monospace',
              fontSize: 13,
              bgcolor: 'action.hover',
            }}
          >
            {code.signature}
          </Box>
        ) : null}

        {code.symbol ? (
          <Typography
            variant="caption"
            color="text.secondary"
            sx={{ fontFamily: 'monospace', wordBreak: 'break-all' }}
          >
            {code.symbol}
          </Typography>
        ) : null}

        {!code.source_available && code.source_unavailable_reason ? (
          <Alert severity="info">{code.source_unavailable_reason}</Alert>
        ) : null}
        {error ? <Alert severity="warning">{error}</Alert> : null}

        {loading && !source ? <CircularProgress size={20} aria-label="Loading source" /> : null}
        {source ? <SourceListing source={source} /> : null}
        {/*
          The scope control follows AVAILABILITY, not a loaded preview. A
          definition longer than the context window is refused at
          `scope=context` while `scope=full` serves it fine, so gating the
          toggle on `source` would answer that failure by removing the one
          control that recovers from it.
        */}
        {code.source_available && (source || error) ? (
          <Stack direction="row" spacing={1} alignItems="center" useFlexGap flexWrap="wrap">
            <Button
              size="small"
              disabled={loading}
              startIcon={scope === 'context' ? <UnfoldMoreIcon /> : <UnfoldLessIcon />}
              onClick={() => setScope(scope === 'context' ? 'full' : 'context')}
            >
              {scope === 'context' ? 'Show full file' : 'Show definition only'}
            </Button>
            {source ? (
              <Typography variant="caption" color="text.secondary">
                Lines {source.start_line}–{source.end_line} of {source.total_lines}
                {source.truncated ? ' · shortened to stay within the preview limit' : ''}
              </Typography>
            ) : null}
          </Stack>
        ) : null}
      </Stack>
    </Box>
  );
}

interface SourceListingProps {
  source: RecordSourceResponse;
}

/**
 * Deliberately a plain monospace listing: no syntax-highlighting dependency,
 * and every line is rendered as text so indexed source can never become
 * markup.
 */
/**
 * Whether a 1-based line is part of the definition.
 *
 * The span's END is exclusive, mirroring the substrate. A multi-line
 * declaration ending at column 1 stops at the START of `end_line`, so that
 * line holds none of it — treating the span as inclusive would highlight one
 * line too many.
 */
export function lineIsDefinition(lineNumber: number, span: SourceSpan | null): boolean {
  if (!span || lineNumber < span.start_line) {
    return false;
  }
  if (lineNumber < span.end_line) {
    return true;
  }
  return lineNumber === span.end_line && span.end_col > 1;
}

export function SourceListing({ source }: SourceListingProps) {
  const lines = source.text.length > 0 ? source.text.split('\n') : [];

  return (
    <Box
      sx={{
        border: 1,
        borderColor: 'divider',
        borderRadius: 1,
        overflowX: 'auto',
        maxHeight: 560,
        overflowY: 'auto',
        bgcolor: 'background.paper',
      }}
    >
      <Box
        component="table"
        sx={{ borderCollapse: 'collapse', fontFamily: 'monospace', fontSize: 13, width: '100%' }}
      >
        <tbody>
          {lines.map((line, index) => {
            const lineNumber = source.start_line + index;
            const isDefinition = lineIsDefinition(lineNumber, source.definition);
            return (
              <Box
                component="tr"
                key={lineNumber}
                data-definition={isDefinition ? 'true' : undefined}
                sx={{ bgcolor: isDefinition ? 'action.selected' : 'transparent' }}
              >
                <Box
                  component="td"
                  sx={{
                    px: 1,
                    textAlign: 'right',
                    color: 'text.disabled',
                    userSelect: 'none',
                    verticalAlign: 'top',
                    width: '1%',
                    whiteSpace: 'nowrap',
                  }}
                >
                  {lineNumber}
                </Box>
                <Box
                  component="td"
                  sx={{ px: 1, whiteSpace: 'pre', verticalAlign: 'top' }}
                >
                  {line}
                </Box>
              </Box>
            );
          })}
        </tbody>
      </Box>
    </Box>
  );
}
