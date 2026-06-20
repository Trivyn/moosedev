import { Box, Typography } from '@mui/material';
import ReactMarkdown from 'react-markdown';
import { ChatMessage as Message } from '../../api/types';

interface ChatMessageProps {
  message: Message;
}

export default function ChatMessage({ message }: ChatMessageProps) {
  const isUser = message.role === 'user';
  return (
    <Box
      sx={{
        display: 'flex',
        justifyContent: isUser ? 'flex-end' : 'flex-start',
        px: 2,
        py: 0.75,
      }}
    >
      <Box
        sx={{
          maxWidth: '78%',
          px: 1.5,
          py: 1,
          border: 1,
          borderColor: isUser ? 'primary.light' : 'divider',
          bgcolor: isUser ? 'rgba(31, 111, 91, 0.08)' : 'background.paper',
          borderRadius: 1,
          overflowWrap: 'anywhere',
          '& p': { my: 0.5 },
          '& pre': { whiteSpace: 'pre-wrap' },
        }}
      >
        <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mb: 0.5 }}>
          {isUser ? 'You' : 'MOOSE'}
        </Typography>
        <ReactMarkdown>{message.content}</ReactMarkdown>
      </Box>
    </Box>
  );
}
