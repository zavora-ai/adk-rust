import { useState, useCallback, useEffect } from 'react';
import { Viewport } from '@xyflow/react';
import type { ProjectSettings } from '../types/project';
import { loadGlobalSettings } from '../types/settings';

export interface CanvasUIState {
  showMinimap: boolean;
  showDataFlowOverlay: boolean;
  showConsole: boolean;
  showTimeline: boolean;
}

/**
 * Hook for managing canvas-specific UI state.
 * Separates canvas UI concerns from project/execution state.
 * Initializes from project settings or global defaults.
 */
export function useCanvasState(projectSettings?: ProjectSettings, onSettingsChange?: (key: string, value: boolean) => void) {
  // Get defaults from global settings
  const globalSettings = loadGlobalSettings();
  
  const defaultUIState: CanvasUIState = {
    showMinimap: projectSettings?.showMinimap ?? globalSettings.showMinimap,
    showDataFlowOverlay: projectSettings?.showDataFlowOverlay ?? globalSettings.showDataFlowOverlay,
    showTimeline: projectSettings?.showTimeline ?? globalSettings.showTimeline,
    showConsole: true,
  };
  
  // Viewport state (managed by ReactFlow, but we track for persistence)
  const [viewport, setViewport] = useState<Viewport>({ x: 0, y: 0, zoom: 1 });
  
  // UI visibility state
  const [uiState, setUIState] = useState<CanvasUIState>(defaultUIState);

  // Update UI state when project settings change
  useEffect(() => {
    if (projectSettings) {
      setUIState(prev => ({
        ...prev,
        showMinimap: projectSettings.showMinimap ?? prev.showMinimap,
        showDataFlowOverlay: projectSettings.showDataFlowOverlay ?? prev.showDataFlowOverlay,
        showTimeline: projectSettings.showTimeline ?? prev.showTimeline,
      }));
    }
  }, [projectSettings?.showMinimap, projectSettings?.showDataFlowOverlay, projectSettings?.showTimeline]);

  // Minimap toggle - also persist to project settings
  const toggleMinimap = useCallback(() => {
    setUIState(s => {
      const newValue = !s.showMinimap;
      onSettingsChange?.('showMinimap', newValue);
      return { ...s, showMinimap: newValue };
    });
  }, [onSettingsChange]);

  // Data flow overlay toggle
  const toggleDataFlowOverlay = useCallback(() => {
    setUIState(s => {
      const newValue = !s.showDataFlowOverlay;
      onSettingsChange?.('showDataFlowOverlay', newValue);
      return { ...s, showDataFlowOverlay: newValue };
    });
  }, [onSettingsChange]);

  // Timeline toggle - also persist to project settings
  const toggleTimeline = useCallback(() => {
    setUIState(s => {
      const newValue = !s.showTimeline;
      onSettingsChange?.('showTimeline', newValue);
      return { ...s, showTimeline: newValue };
    });
  }, [onSettingsChange]);

  // Console toggle
  const toggleConsole = useCallback(() => {
    setUIState(s => ({ ...s, showConsole: !s.showConsole }));
  }, []);

  // Set specific UI state
  const setShowMinimap = useCallback((show: boolean) => {
    setUIState(s => ({ ...s, showMinimap: show }));
    onSettingsChange?.('showMinimap', show);
  }, [onSettingsChange]);

  const setShowDataFlowOverlay = useCallback((show: boolean) => {
    setUIState(s => ({ ...s, showDataFlowOverlay: show }));
    onSettingsChange?.('showDataFlowOverlay', show);
  }, [onSettingsChange]);

  const setShowTimeline = useCallback((show: boolean) => {
    setUIState(s => ({ ...s, showTimeline: show }));
    onSettingsChange?.('showTimeline', show);
  }, [onSettingsChange]);

  const setShowConsole = useCallback((show: boolean) => {
    setUIState(s => ({ ...s, showConsole: show }));
  }, []);

  // Handle viewport changes from ReactFlow
  const onViewportChange = useCallback((newViewport: Viewport) => {
    setViewport(newViewport);
  }, []);

  return {
    // Viewport
    viewport,
    setViewport,
    onViewportChange,
    
    // UI state
    showMinimap: uiState.showMinimap,
    showDataFlowOverlay: uiState.showDataFlowOverlay,
    showTimeline: uiState.showTimeline,
    showConsole: uiState.showConsole,
    
    // Toggles
    toggleMinimap,
    toggleDataFlowOverlay,
    toggleTimeline,
    toggleConsole,
    
    // Setters
    setShowMinimap,
    setShowDataFlowOverlay,
    setShowTimeline,
    setShowConsole,
  };
}
