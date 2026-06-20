import { Box, Chip, Typography } from '@mui/material';
import { FocusEntry } from '../../api/types';
import { shortName } from '../graph/graphUtils';

interface FocusStackProps {
  focus: FocusEntry[];
}

export default function FocusStack({ focus }: FocusStackProps) {
  if (focus.length === 0) {
    return (
      <Typography variant="body2" color="text.secondary" sx={{ p: 2 }}>
        No active focus entries.
      </Typography>
    );
  }
  return (
    <Box sx={{ p: 1.5, display: 'flex', gap: 1, flexWrap: 'wrap', alignContent: 'flex-start' }}>
      {focus.map((entry) => (
        <Chip
          key={entry.iri}
          size="small"
          label={`${entry.label || shortName(entry.iri)} ${Math.round(entry.salience * 100)}%`}
          title={entry.iri}
        />
      ))}
    </Box>
  );
}
