import { ReactNode } from 'react';
import { Box, ButtonBase, Divider, Stack, Typography } from '@mui/material';
import { HealthResponse } from '../../api/types';

export type PageKey = 'chat' | 'sparql';

interface AppShellProps {
  children: ReactNode;
  page: PageKey;
  onPageChange: (page: PageKey) => void;
  nav: Array<{ key: PageKey; label: string; icon: ReactNode }>;
  health: HealthResponse | null;
}

export default function AppShell({ children, page, onPageChange, nav, health }: AppShellProps) {
  return (
    <Box sx={{ height: '100%', display: 'flex', overflow: 'hidden' }}>
      <Box
        component="nav"
        sx={{
          width: 220,
          flexShrink: 0,
          borderRight: 1,
          borderColor: 'divider',
          bgcolor: 'background.paper',
          display: 'flex',
          flexDirection: 'column',
        }}
      >
        <Box sx={{ px: 2, py: 1.5 }}>
          <Typography variant="h6">MOOSEDev</Typography>
          <Typography variant="caption" color="text.secondary">
            Project knowledge graph
          </Typography>
        </Box>
        <Divider />
        <Stack sx={{ p: 1 }} spacing={0.5}>
          {nav.map((item) => (
            <ButtonBase
              key={item.key}
              onClick={() => onPageChange(item.key)}
              sx={{
                justifyContent: 'flex-start',
                gap: 1,
                px: 1.25,
                py: 1,
                borderRadius: 1,
                color: page === item.key ? 'primary.main' : 'text.primary',
                bgcolor: page === item.key ? 'rgba(31, 111, 91, 0.10)' : 'transparent',
                '&:hover': { bgcolor: 'rgba(0, 0, 0, 0.04)' },
              }}
            >
              {item.icon}
              <Typography variant="body2">{item.label}</Typography>
            </ButtonBase>
          ))}
        </Stack>
        <Box sx={{ mt: 'auto', p: 1.5 }}>
          <Typography variant="caption" color="text.secondary" sx={{ display: 'block' }}>
            {health ? `v${health.version}` : 'Disconnected'}
          </Typography>
          {health && (
            <Typography
              variant="caption"
              color="text.secondary"
              title={health.data_dir}
              sx={{ display: 'block', overflow: 'hidden', textOverflow: 'ellipsis' }}
            >
              {health.data_dir}
            </Typography>
          )}
        </Box>
      </Box>
      <Box component="main" sx={{ flex: 1, minWidth: 0, overflow: 'hidden' }}>
        {children}
      </Box>
    </Box>
  );
}
