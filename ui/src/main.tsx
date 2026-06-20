import React, { useEffect, useMemo, useState } from 'react';
import ReactDOM from 'react-dom/client';
import { CssBaseline, ThemeProvider } from '@mui/material';
import App from './App';
import { createMooseTheme, MooseThemeMode } from './styles/theme';
import './styles/global.css';

const THEME_STORAGE_KEY = 'moosedev.theme';

// Keep the UI preference local to the browser; MOOSE session state remains server-owned.
function storedThemeMode(): MooseThemeMode {
  const stored = window.localStorage.getItem(THEME_STORAGE_KEY);
  return stored === 'dark' || stored === 'light' ? stored : 'light';
}

function Root() {
  const [mode, setMode] = useState<MooseThemeMode>(storedThemeMode);
  const theme = useMemo(() => createMooseTheme(mode), [mode]);

  useEffect(() => {
    document.documentElement.dataset.theme = mode;
  }, [mode]);

  const toggleMode = () => {
    setMode((current) => {
      const next = current === 'dark' ? 'light' : 'dark';
      window.localStorage.setItem(THEME_STORAGE_KEY, next);
      return next;
    });
  };

  return (
    <ThemeProvider theme={theme}>
      <CssBaseline />
      <App themeMode={mode} onToggleThemeMode={toggleMode} />
    </ThemeProvider>
  );
}

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
