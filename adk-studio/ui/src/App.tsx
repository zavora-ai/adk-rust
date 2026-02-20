import { useCallback, useEffect, useState } from 'react';
import { ReactFlowProvider } from '@xyflow/react';
import { useStore } from './store';
import { ProjectList } from './components/Projects/ProjectList';
import { Canvas } from './components/Canvas/Canvas';
import { ThemeProvider, ThemeToggle } from './components/Theme';
import { WalkthroughModal, SettingsModal, TemplateWalkthroughModal } from './components/Overlays';
import { useWalkthrough } from './hooks/useWalkthrough';
import { useTheme } from './hooks/useTheme';
import { useVSCodeThemeSync } from './hooks/useVSCodeThemeSync';
import { loadGlobalSettings } from './types/settings';
import { api } from './api/client';
import type { ProjectSettings } from './types/project';

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
  const [deploying, setDeploying] = useState(false);
  const [deployMessage, setDeployMessage] = useState<string | null>(null);

  // Listen for live VS Code theme changes via postMessage
  useVSCodeThemeSync();
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

  const handleDeploy = useCallback(async () => {
    if (!currentProject || deploying) return;
    setDeploying(true);
    setDeployMessage(null);
    try {
      const result = await api.projects.deploy(currentProject.id, {
        register: true,
        openSpatialOs: true,
      });
      if (result.openUrl) {
        window.open(result.openUrl, '_blank', 'noopener,noreferrer');
      }
      if (result.registration.success) {
        setDeployMessage(`Deployed. Manifest: ${result.manifestPath}`);
      } else {
        setDeployMessage(
          `Manifest created. Spatial OS registration failed: ${result.registration.message}`
        );
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Unknown error';
      setDeployMessage(`Deploy failed: ${message}`);
    } finally {
      setDeploying(false);
    }
  }, [currentProject, deploying]);

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
              <img src="https://adk-rust.com/icon.svg" alt="ADK" className="w-7 h-7" /> ADK Studio
            </h1>
            {currentProject && (
              <span className="ml-4" style={{ color: 'var(--text-secondary)' }}>/ {currentProject.name}</span>
            )}
          </div>
          <div className="flex items-center gap-2">
            <button
              onClick={handleDeploy}
              disabled={!currentProject || deploying}
              className="px-3 py-1.5 rounded text-xs font-semibold border transition-opacity disabled:opacity-50"
              style={{
                borderColor: 'var(--accent-primary)',
                color: 'var(--accent-primary)',
                backgroundColor: 'transparent',
              }}
              title="Create deploy manifest and register with ADK Spatial OS"
            >
              {deploying ? 'Deploying...' : 'Deploy'}
            </button>
            <button
              onClick={() => setShowGlobalSettings(true)}
              className="p-1.5 rounded hover:opacity-80 transition-opacity"
              style={{ color: 'var(--text-secondary)' }}
              title={currentProject ? 'Project Settings' : 'Settings'}
            >
              ⚙️
            </button>
            <ThemeToggle size={20} />
          </div>
        </header>
        <main className="flex-1 overflow-hidden" style={{ backgroundColor: 'var(--bg-canvas)' }}>
          {deployMessage && (
            <div
              className="px-4 py-2 text-xs border-b"
              style={{
                backgroundColor: 'var(--surface-panel)',
                borderColor: 'var(--border-default)',
                color: 'var(--text-secondary)',
              }}
            >
              {deployMessage}
            </div>
          )}
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
        
        {/* Settings Modal */}
        {showGlobalSettings && (
          <SettingsModal
            settings={currentProject?.settings}
            projectName={currentProject?.name}
            projectDescription={currentProject?.description}
            projectId={currentProject?.id}
            onSave={currentProject ? (settings: ProjectSettings, name: string, description: string) => {
              const { updateProjectSettings, updateProjectMeta } = useStore.getState();
              updateProjectMeta(name, description);
              updateProjectSettings(settings);
            } : undefined}
            onClose={() => setShowGlobalSettings(false)}
          />
        )}
        
        {/* Template Walkthrough Modal - shows after loading a template */}
        <TemplateWalkthroughModal 
          onAction={(actionType, templateId) => {
            // Handle walkthrough actions
            if (actionType === 'open-env') {
              setShowGlobalSettings(true);
            } else if (actionType === 'open-docs') {
              // Open template documentation
              window.open(`https://github.com/zavora-ai/adk-rust/blob/main/adk-studio/ui/src/components/Templates/docs/${templateId}.md`, '_blank');
            }
            // run-workflow and highlight-node can be handled by the Canvas component
          }}
        />
      </div>
    </ThemeProvider>
  );
}
