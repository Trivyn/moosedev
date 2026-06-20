import { Box, Card, CardContent, Chip, Divider, IconButton, Stack, Typography } from '@mui/material';
import CloseIcon from '@mui/icons-material/Close';
import { GraphEdge, GraphNode, GraphProperty, QueryValue } from '../../api/types';
import { shortName } from './graphUtils';

interface GraphDetailsPanelProps {
  node?: GraphNode | null;
  edge?: GraphEdge | null;
  onClose: () => void;
}

function valueLabel(value: QueryValue): string {
  return value.type === 'uri' || value.type === 'bnode' ? shortName(value.value) : value.value;
}

function PropertyList({ properties = [] }: { properties?: GraphProperty[] }) {
  if (properties.length === 0) {
    return (
      <Typography variant="body2" color="text.secondary">
        No recorded properties.
      </Typography>
    );
  }

  return (
    <Stack spacing={1}>
      {properties.map((property) => (
        <Box key={property.predicate}>
          <Typography variant="caption" color="text.secondary">
            {shortName(property.predicate)}
          </Typography>
          {property.values.map((value, index) => (
            <Typography
              key={`${property.predicate}-${index}`}
              variant="body2"
              sx={{
                fontFamily: value.type === 'uri' || value.type === 'bnode' ? 'monospace' : 'inherit',
                whiteSpace: 'pre-wrap',
                wordBreak: 'break-word',
              }}
            >
              {valueLabel(value)}
              {value.lang ? ` @${value.lang}` : ''}
              {value.datatype && value.datatype !== 'http://www.w3.org/2001/XMLSchema#string'
                ? ` (${shortName(value.datatype)})`
                : ''}
            </Typography>
          ))}
        </Box>
      ))}
    </Stack>
  );
}

export default function GraphDetailsPanel({ node, edge, onClose }: GraphDetailsPanelProps) {
  const selected = node ?? edge;
  if (!selected) return null;

  return (
    <Card
      sx={{
        position: 'absolute',
        top: 12,
        right: 12,
        width: 360,
        maxWidth: 'calc(100% - 24px)',
        maxHeight: 'calc(100% - 24px)',
        overflow: 'auto',
        zIndex: 5,
        bgcolor: (theme) =>
          theme.palette.mode === 'dark' ? 'rgba(24, 32, 32, 0.96)' : 'rgba(255, 255, 255, 0.96)',
        backdropFilter: 'blur(8px)',
        boxShadow: 4,
      }}
    >
      <CardContent>
        <Box sx={{ display: 'flex', gap: 1, alignItems: 'flex-start', mb: 1.5 }}>
          <Box sx={{ minWidth: 0, flex: 1 }}>
            <Typography variant="subtitle1" sx={{ fontWeight: 650, wordBreak: 'break-word' }}>
              {selected.label}
            </Typography>
            <Stack direction="row" spacing={0.75} sx={{ mt: 0.75, flexWrap: 'wrap' }}>
              <Chip size="small" label={node ? 'Node' : 'Edge'} />
              <Chip size="small" variant="outlined" label={selected.type} />
            </Stack>
          </Box>
          <IconButton size="small" onClick={onClose} aria-label="Close graph details">
            <CloseIcon fontSize="small" />
          </IconButton>
        </Box>

        <Typography variant="caption" color="text.secondary">
          IRI
        </Typography>
        <Typography variant="body2" sx={{ fontFamily: 'monospace', wordBreak: 'break-all' }}>
          {selected.id}
        </Typography>

        {edge && (
          <Box sx={{ mt: 1.5 }}>
            <Typography variant="caption" color="text.secondary">
              Connection
            </Typography>
            <Typography variant="body2" sx={{ wordBreak: 'break-word' }}>
              {shortName(edge.source)} {'->'} {shortName(edge.target)}
            </Typography>
            {edge.predicate && (
              <Typography variant="body2" sx={{ fontFamily: 'monospace', wordBreak: 'break-all' }}>
                {edge.predicate}
              </Typography>
            )}
          </Box>
        )}

        <Divider sx={{ my: 1.5 }} />
        <Typography variant="subtitle2" gutterBottom>
          Properties
        </Typography>
        <PropertyList properties={selected.properties} />
      </CardContent>
    </Card>
  );
}
