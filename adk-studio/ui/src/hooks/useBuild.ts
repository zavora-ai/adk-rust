import { useState, useCallback, useRef, useEffect } from 'react';
import { api, GeneratedProject } from '../api/client';
import type { AutobuildTriggers } from '../types/project';
import { loadGlobalSettings } from '../types/settings';

export interface BuildOutput {
  success: boolean;
  output: string;
  path: string | null;
}

// Autobuild trigger types
export type AutobuildTriggerType = 
  | 'onAgentAdd' 
  | 'onAgentDelete' 
  | 'onAgentUpdate' 
  | 'onToolAdd' 
  | 'onToolUpdate' 
  | 'onEdgeAdd' 
  | 'onEdgeDelete';

// Get default autobuild triggers from global settings
function getDefaultAutobuildTriggers(): AutobuildTriggers {
  const globalSettings = loadGlobalSettings();
  return globalSettings.autobuildTriggers;
}

// Persist autobuild preference in localStorage
const AUTOBUILD_KEY = 'adk-studio-autobuild';

function getStoredAutobuild(): boolean {
  try {
    const stored = localStorage.getItem(AUTOBUILD_KEY);
    if (stored !== null) {
      return stored === 'true';
    }
    // Fall back to global settings default
    return loadGlobalSettings().autobuildEnabled;
  } catch {
    return true;
  }
}

function setStoredAutobuild(value: boolean): void {
  try {
    localStorage.setItem(AUTOBUILD_KEY, String(value));
  } catch {
    // Ignore storage errors
  }
}

/**
 * Hook for managing build and compile operations.
 * Includes autobuild functionality that triggers builds automatically on project changes.
 * 
 * @param projectId - The current project ID
 * @param autobuildTriggers - Optional trigger configuration from project settings
 * @param projectAutobuildEnabled - Optional project-level autobuild enabled setting (overrides global)
 * @param canBuild - Optional function to check if build is possible (e.g., has agents and edges)
 */
export function useBuild(
  projectId: string | undefined, 
  autobuildTriggers?: AutobuildTriggers,
  projectAutobuildEnabled?: boolean,
  canBuild?: () => boolean
) {
  const [building, setBuilding] = useState(false);
  const [buildOutput, setBuildOutput] = useState<BuildOutput | null>(null);
  const [builtBinaryPath, setBuiltBinaryPath] = useState<string | null>(null);
  const [compiledCode, setCompiledCode] = useState<GeneratedProject | null>(null);
  
  // Autobuild state - use project setting if defined, otherwise use stored/global
  const [autobuildEnabled, setAutobuildEnabled] = useState(() => {
    if (projectAutobuildEnabled !== undefined) return projectAutobuildEnabled;
    return getStoredAutobuild();
  });
  const [isAutobuild, setIsAutobuild] = useState(false);
  const autobuildTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const eventSourceRef = useRef<EventSource | null>(null);

  // Sync autobuild state with project settings when they change
  useEffect(() => {
    if (projectAutobuildEnabled !== undefined) {
      setAutobuildEnabled(projectAutobuildEnabled);
    }
  }, [projectAutobuildEnabled]);

  // Compile project to view generated code
  const compile = useCallback(async () => {
    if (!projectId) return null;
    try {
      const code = await api.projects.compile(projectId);
      setCompiledCode(code);
      return code;
    } catch (e) {
      const error = e as Error;
      alert('Compile failed: ' + error.message);
      return null;
    }
  }, [projectId]);

  // Core build function (used by both manual and auto build)
  // For autobuild, we don't set buildOutput to avoid showing modal
  const executeBuild = useCallback(async (isAuto: boolean) => {
    if (!projectId || building) return;
    
    // Skip build if canvas is empty (no agents or edges)
    if (canBuild && !canBuild()) {
      // Silently skip - nothing to build yet
      return;
    }
    
    setBuilding(true);
    setIsAutobuild(isAuto);
    
    // Only set buildOutput for manual builds (to show modal)
    // For autobuild, we track progress internally but don't show modal
    if (!isAuto) {
      setBuildOutput({ success: false, output: '', path: null });
    }
    
    // Close any existing event source
    if (eventSourceRef.current) {
      eventSourceRef.current.close();
    }
    
    const es = new EventSource(`/api/projects/${projectId}/build-stream`);
    eventSourceRef.current = es;
    let output = '';
    
    es.addEventListener('status', (e) => {
      output += e.data + '\n';
      if (!isAuto || buildOutputRef.current) {
        setBuildOutput({ success: false, output, path: null });
      }
    });
    
    es.addEventListener('output', (e) => {
      output += e.data + '\n';
      if (!isAuto || buildOutputRef.current) {
        setBuildOutput({ success: false, output, path: null });
      }
    });
    
    es.addEventListener('done', (e) => {
      if (!isAuto || buildOutputRef.current) {
        setBuildOutput({ success: true, output, path: e.data });
      }
      setBuiltBinaryPath(e.data);
      setBuilding(false);
      setIsAutobuild(false);
      es.close();
      eventSourceRef.current = null;
    });
    
    es.addEventListener('error', (e) => {
      output += '\nError: ' + ((e as MessageEvent).data || 'Build failed');
      // Always show errors, even for autobuild
      setBuildOutput({ success: false, output, path: null });
      setBuilding(false);
      setIsAutobuild(false);
      es.close();
      eventSourceRef.current = null;
    });
    
    es.onerror = () => {
      setBuilding(false);
      setIsAutobuild(false);
      es.close();
      eventSourceRef.current = null;
    };
  }, [projectId, building, canBuild]);
  
  // Track if user has requested to see build output
  const buildOutputRef = useRef<boolean>(false);
  
  // Update ref when buildOutput changes
  useEffect(() => {
    buildOutputRef.current = buildOutput !== null;
  }, [buildOutput]);

  // Manual build - shows modal
  const build = useCallback(async () => {
    await executeBuild(false);
  }, [executeBuild]);

  // Autobuild - runs in background, shows modal only if user clicks button
  // Accepts optional trigger type to check against configured triggers
  const triggerAutobuild = useCallback((triggerType?: AutobuildTriggerType) => {
    if (!autobuildEnabled || building) return;
    
    // If a trigger type is specified, check if it's enabled in settings
    if (triggerType) {
      const triggers = autobuildTriggers || getDefaultAutobuildTriggers();
      if (!triggers[triggerType]) {
        // This trigger type is disabled, skip autobuild
        return;
      }
    }
    
    // Cancel any pending autobuild
    if (autobuildTimerRef.current) {
      clearTimeout(autobuildTimerRef.current);
    }
    
    // Debounce autobuild by 1 second to avoid rapid rebuilds
    autobuildTimerRef.current = setTimeout(() => {
      executeBuild(true);
    }, 1000);
  }, [autobuildEnabled, building, executeBuild, autobuildTriggers]);

  // Clear build output (for closing modal)
  const clearBuildOutput = useCallback(() => {
    setBuildOutput(null);
  }, []);

  // Clear compiled code (for closing modal)
  const clearCompiledCode = useCallback(() => {
    setCompiledCode(null);
  }, []);

  // Invalidate build when project changes - triggers autobuild if enabled
  // Accepts optional trigger type to check against configured triggers
  const invalidateBuild = useCallback((triggerType?: AutobuildTriggerType) => {
    setBuiltBinaryPath(null);
    triggerAutobuild(triggerType);
  }, [triggerAutobuild]);

  // Toggle autobuild
  const toggleAutobuild = useCallback(() => {
    const newValue = !autobuildEnabled;
    setAutobuildEnabled(newValue);
    setStoredAutobuild(newValue);
    
    // If enabling autobuild and no binary exists, trigger build
    if (newValue && !builtBinaryPath && !building) {
      triggerAutobuild();
    }
  }, [autobuildEnabled, builtBinaryPath, building, triggerAutobuild]);

  // Show build modal (for when user clicks during autobuild)
  const showBuildProgress = useCallback(() => {
    if (building && isAutobuild) {
      // User clicked during autobuild - show the modal with current progress
      setBuildOutput({ success: false, output: 'Build in progress...', path: null });
    }
  }, [building, isAutobuild]);

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (autobuildTimerRef.current) {
        clearTimeout(autobuildTimerRef.current);
      }
      if (eventSourceRef.current) {
        eventSourceRef.current.close();
      }
    };
  }, []);

  return {
    // State
    building,
    buildOutput,
    builtBinaryPath,
    compiledCode,
    autobuildEnabled,
    isAutobuild,
    
    // Actions
    build,
    compile,
    clearBuildOutput,
    clearCompiledCode,
    invalidateBuild,
    toggleAutobuild,
    showBuildProgress,
    
    // Computed
    needsBuild: !builtBinaryPath,
  };
}
