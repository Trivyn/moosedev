import { PaletteMode } from '@mui/material';
import { createTheme } from '@mui/material/styles';

export type MooseThemeMode = PaletteMode;

export function createMooseTheme(mode: MooseThemeMode) {
  const isDark = mode === 'dark';

  return createTheme({
    palette: {
      mode,
      primary: {
        main: isDark ? '#66c2a8' : '#1f6f5b',
      },
      secondary: {
        main: isDark ? '#d7b95c' : '#725a16',
      },
      background: {
        default: isDark ? '#111616' : '#f7f8f8',
        paper: isDark ? '#182020' : '#ffffff',
      },
      divider: isDark ? 'rgba(255, 255, 255, 0.14)' : 'rgba(0, 0, 0, 0.12)',
    },
    shape: {
      borderRadius: 6,
    },
    typography: {
      fontFamily:
        'Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
      h5: {
        fontWeight: 650,
        letterSpacing: 0,
      },
      h6: {
        fontWeight: 650,
        letterSpacing: 0,
      },
      button: {
        textTransform: 'none',
        letterSpacing: 0,
      },
    },
    components: {
      MuiButton: {
        styleOverrides: {
          root: {
            borderRadius: 6,
          },
        },
      },
      MuiIconButton: {
        styleOverrides: {
          root: {
            borderRadius: 6,
          },
        },
      },
      MuiTab: {
        styleOverrides: {
          root: {
            minHeight: 40,
            letterSpacing: 0,
            textTransform: 'none',
          },
        },
      },
    },
  });
}
