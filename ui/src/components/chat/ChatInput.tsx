import { KeyboardEvent, useState } from 'react';
import { Box, IconButton, TextField, Tooltip } from '@mui/material';
import SendIcon from '@mui/icons-material/Send';

interface ChatInputProps {
  disabled?: boolean;
  onSend: (message: string) => void;
}

export default function ChatInput({ disabled = false, onSend }: ChatInputProps) {
  const [value, setValue] = useState('');

  const send = () => {
    const message = value.trim();
    if (!message || disabled) return;
    onSend(message);
    setValue('');
  };

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      send();
    }
  };

  return (
    <Box sx={{ display: 'flex', gap: 1, p: 1, borderTop: 1, borderColor: 'divider' }}>
      <TextField
        fullWidth
        multiline
        maxRows={5}
        size="small"
        placeholder="Ask about the project knowledge graph"
        value={value}
        disabled={disabled}
        onKeyDown={onKeyDown}
        onChange={(event) => setValue(event.target.value)}
      />
      <Tooltip title="Send">
        <span>
          <IconButton color="primary" disabled={disabled || !value.trim()} onClick={send}>
            <SendIcon />
          </IconButton>
        </span>
      </Tooltip>
    </Box>
  );
}
