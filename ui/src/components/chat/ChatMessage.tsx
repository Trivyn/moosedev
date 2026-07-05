import { Box, Typography } from '@mui/material';
import ReactMarkdown from 'react-markdown';
import { ChatMessage as Message, ClarificationReply, ClarificationRequest } from '../../api/types';
import ClarificationCard from './ClarificationCard';

/** Chat transcript entry with UI-only clarification state layered on top of the
 * wire `ChatMessage`. Strip these fields before sending (see
 * `toWireMessages` in ChatPage). */
export interface UIChatMessage extends Message {
  /** Present when this assistant turn is a MOOSE clarification request. */
  clarification?: ClarificationRequest;
  /** Set once the user has replied, so the card renders disabled. */
  clarificationAnswered?: boolean;
}

interface ChatMessageProps {
  message: UIChatMessage;
  onClarificationReply?: (reply: ClarificationReply) => void;
}

export default function ChatMessage({ message, onClarificationReply }: ChatMessageProps) {
  const isUser = message.role === 'user';
  const isClarification = !!message.clarification;
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
          // The self-framed clarification card gets more room and no bubble
          // chrome, so its border isn't doubled up.
          maxWidth: isClarification ? '92%' : '78%',
          width: isClarification ? '100%' : 'auto',
          px: isClarification ? 0 : 1.5,
          py: isClarification ? 0 : 1,
          border: isClarification ? 0 : 1,
          borderColor: isUser ? 'primary.light' : 'divider',
          bgcolor: isClarification
            ? 'transparent'
            : isUser
              ? 'rgba(31, 111, 91, 0.08)'
              : 'background.paper',
          borderRadius: 1,
          overflowWrap: 'anywhere',
          '& p': { my: 0.5 },
          '& pre': { whiteSpace: 'pre-wrap' },
        }}
      >
        <Typography
          variant="caption"
          color="text.secondary"
          sx={{ display: 'block', mb: 0.5, px: isClarification ? 0.5 : 0 }}
        >
          {isUser ? 'You' : 'MOOSE'}
        </Typography>
        {message.clarification ? (
          <ClarificationCard
            request={message.clarification}
            onReply={(reply) => onClarificationReply?.(reply)}
            disabled={message.clarificationAnswered}
          />
        ) : (
          <ReactMarkdown>{message.content}</ReactMarkdown>
        )}
      </Box>
    </Box>
  );
}
