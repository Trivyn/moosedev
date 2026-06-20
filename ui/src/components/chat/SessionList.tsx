import { Box, IconButton, List, ListItemButton, ListItemText, Tooltip, Typography } from '@mui/material';
import AddIcon from '@mui/icons-material/Add';
import DeleteIcon from '@mui/icons-material/Delete';
import { ChatSessionSummary } from '../../api/types';

interface SessionListProps {
  sessions: ChatSessionSummary[];
  selectedId?: string;
  onNew: () => void;
  onSelect: (sessionId: string) => void;
  onDelete: (sessionId: string) => void;
}

export default function SessionList({
  sessions,
  selectedId,
  onNew,
  onSelect,
  onDelete,
}: SessionListProps) {
  return (
    <Box sx={{ width: 260, borderRight: 1, borderColor: 'divider', overflow: 'hidden' }}>
      <Box sx={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', p: 1 }}>
        <Typography variant="subtitle2">Sessions</Typography>
        <Tooltip title="New chat">
          <IconButton size="small" onClick={onNew}>
            <AddIcon fontSize="small" />
          </IconButton>
        </Tooltip>
      </Box>
      <List dense sx={{ overflow: 'auto', height: 'calc(100% - 48px)' }}>
        {sessions.map((session) => (
          <ListItemButton
            key={session.session_id}
            selected={session.session_id === selectedId}
            onClick={() => onSelect(session.session_id)}
            sx={{ gap: 1 }}
          >
            <ListItemText
              primary={session.last_user_message || 'New conversation'}
              secondary={`${session.turn_count} turns`}
              primaryTypographyProps={{
                noWrap: true,
                variant: 'body2',
              }}
            />
            <Tooltip title="Delete">
              <IconButton
                size="small"
                onClick={(event) => {
                  event.stopPropagation();
                  onDelete(session.session_id);
                }}
              >
                <DeleteIcon fontSize="small" />
              </IconButton>
            </Tooltip>
          </ListItemButton>
        ))}
      </List>
    </Box>
  );
}
