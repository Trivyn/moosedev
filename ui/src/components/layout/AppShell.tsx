import { ReactNode } from 'react';
import { alpha, Box, ButtonBase, Divider, IconButton, Stack, Tooltip, Typography } from '@mui/material';
import DarkModeIcon from '@mui/icons-material/DarkMode';
import LightModeIcon from '@mui/icons-material/LightMode';
import { HealthResponse } from '../../api/types';
import { MooseThemeMode } from '../../styles/theme';

export type PageKey =
  | 'chat'
  | 'adrs'
  | 'requirements'
  | 'lessons'
  | 'constraints'
  | 'sparql'
  | 'transfer';

interface AppShellProps {
  children: ReactNode;
  page: PageKey;
  onPageChange: (page: PageKey) => void;
  nav: Array<{ key: PageKey; label: string; icon: ReactNode }>;
  health: HealthResponse | null;
  themeMode: MooseThemeMode;
  onToggleThemeMode: () => void;
}

export default function AppShell({
  children,
  page,
  onPageChange,
  nav,
  health,
  themeMode,
  onToggleThemeMode,
}: AppShellProps) {
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
        <Box sx={{ px: 2, py: 1.5, display: 'flex', gap: 1, alignItems: 'flex-start' }}>
          <Box sx={{ minWidth: 0, flex: 1 }}>
            <Typography variant="h6">MOOSEDev</Typography>
            <Typography
              variant="body2"
              title={health?.project_root ?? undefined}
              sx={{ display: 'block', fontWeight: 650, overflow: 'hidden', textOverflow: 'ellipsis' }}
            >
              {health?.project_name ?? 'Project'}
            </Typography>
            {health && (
              <Typography
                variant="caption"
                color="text.secondary"
                title={health.project_root}
                sx={{ display: 'block', overflow: 'hidden', textOverflow: 'ellipsis' }}
              >
                {health.project_root}
              </Typography>
            )}
          </Box>
          <Tooltip title={themeMode === 'dark' ? 'Use light mode' : 'Use dark mode'}>
            <IconButton size="small" onClick={onToggleThemeMode} aria-label="Toggle color mode">
              {themeMode === 'dark' ? <LightModeIcon fontSize="small" /> : <DarkModeIcon fontSize="small" />}
            </IconButton>
          </Tooltip>
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
                bgcolor: page === item.key ? (theme) => alpha(theme.palette.primary.main, 0.12) : 'transparent',
                '&:hover': { bgcolor: (theme) => alpha(theme.palette.primary.main, 0.08) },
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
