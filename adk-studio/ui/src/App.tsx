import { useEffect, useState } from 'react';
import { ReactFlowProvider } from '@xyflow/react';
import { useStore } from './store';
import { ProjectList } from './components/Projects/ProjectList';
import { Canvas } from './components/Canvas/Canvas';
import { ThemeProvider, ThemeToggle } from './components/Theme';
import { WalkthroughModal, GlobalSettingsModal } from './components/Overlays';
import { useWalkthrough } from './hooks/useWalkthrough';
import { useTheme } from './hooks/useTheme';
import { loadGlobalSettings } from './types/settings';

// Component to apply theme from global settings
function ThemeInitializer() {
  const { setMode } = useTheme();
  
  useEffect(() => {
    const globalSettings = loadGlobalSettings();
    if (globalSettings.theme === 'system') {
      // Use system preference
      const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
      setMode(prefersDark ? 'dark' : 'light');
    } else {
      setMode(globalSettings.theme);
    }
  }, [setMode]);
  
  return null;
}

export default function App() {
  const { currentProject, fetchProjects } = useStore();
  const [showGlobalSettings, setShowGlobalSettings] = useState(false);
  const { 
    isVisible: showWalkthrough, 
    complete: completeWalkthrough, 
    skip: skipWalkthrough, 
    hide: hideWalkthrough,
    shouldShowOnFirstRun,
    show: openWalkthrough,
  } = useWalkthrough();

  useEffect(() => {
    fetchProjects();
  }, [fetchProjects]);

  // Show walkthrough on first run
  useEffect(() => {
    if (shouldShowOnFirstRun()) {
      openWalkthrough();
    }
  }, [shouldShowOnFirstRun, openWalkthrough]);

  return (
    <ThemeProvider>
      <ThemeInitializer />
      <div className="h-screen flex flex-col" style={{ backgroundColor: 'var(--bg-primary)' }}>
        <header 
          className="h-12 border-b flex items-center justify-between px-4"
          style={{ 
            backgroundColor: 'var(--surface-panel)', 
            borderColor: 'var(--border-default)',
            color: 'var(--text-primary)'
          }}
        >
          <div className="flex items-center">
            <h1 className="text-lg font-bold flex items-center gap-2">
              <span className="text-2xl">🚀</span> ADK Studio
            </h1>
            {currentProject && (
              <span className="ml-4" style={{ color: 'var(--text-secondary)' }}>/ {currentProject.name}</span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={() => setShowGlobalSettings(true)}
              className="p-1.5 rounded hover:opacity-80 transition-opacity"
              style={{ color: 'var(--text-secondary)' }}
              title="Global Settings"
            >
              ⚙️
            </button>
            <ThemeToggle size={20} />
          </div>
        </header>
        <main className="flex-1 overflow-hidden" style={{ backgroundColor: 'var(--bg-canvas)' }}>
          {currentProject ? (
            <ReactFlowProvider>
              <Canvas />
            </ReactFlowProvider>
          ) : (
            <ProjectList />
          )}
        </main>
        
        {/* Walkthrough Modal - shows on first run or when triggered from Help menu */}
        {showWalkthrough && (
          <WalkthroughModal
            onComplete={completeWalkthrough}
            onSkip={skipWalkthrough}
            onClose={hideWalkthrough}
          />
        )}
        
        {/* Global Settings Modal */}
        {showGlobalSettings && (
          <GlobalSettingsModal onClose={() => setShowGlobalSettings(false)} />
        )}
      </div>
    </ThemeProvider>
  );
}
