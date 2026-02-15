import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { ErrorBoundary } from './components/ErrorBoundary';
import { initVSCodeTheme } from './hooks/useVSCodeThemeSync';
import './index.css';

// Apply VS Code theme from URL param BEFORE React renders to avoid flash.
// This is synchronous and sets data-theme + localStorage immediately.
initVSCodeTheme();

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>
);
