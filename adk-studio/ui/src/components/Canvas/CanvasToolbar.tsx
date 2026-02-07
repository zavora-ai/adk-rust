import { useLayout } from '../../hooks/useLayout';
import { useViewport } from '@xyflow/react';
import { Tooltip } from '../Overlays/Tooltip';

interface CanvasToolbarProps {
  onFitView: () => void;
  /** v2.0: Data flow overlay toggle */
  showDataFlowOverlay?: boolean;
  onToggleDataFlowOverlay?: () => void;
  /** v2.0: Run/Stop execution controls (Requirements 10.8, 10.9) */
  isRunning?: boolean;
  onRun?: () => void;
  onStop?: () => void;
  /** v2.0: Minimap toggle (Requirements 8.1, 8.2) */
  showMinimap?: boolean;
  onToggleMinimap?: () => void;
  /** Build state - Run is disabled when not built or during build */
  isBuilt?: boolean;
  isBuilding?: boolean;
}

export function CanvasToolbar({ 
  onFitView, 
  showDataFlowOverlay, 
  onToggleDataFlowOverlay,
  isRunning,
  onRun,
  onStop,
  showMinimap,
  onToggleMinimap,
  isBuilt,
  isBuilding,
}: CanvasToolbarProps) {
  const { 
    layoutDirection, 
    toggleDirection,
    snapToGrid,
    setSnapToGrid,
    gridSize,
    setGridSize,
  } = useLayout();
  const viewport = useViewport();
  const zoomPercent = Math.round(viewport.zoom * 100);
  
  const isHorizontal = layoutDirection === 'LR';
  
  // Use CSS variables for theme-aware styling
  const buttonStyle: React.CSSProperties = {
    backgroundColor: 'var(--surface-card)',
    borderColor: 'var(--border-default)',
    color: 'var(--text-primary)',
  };
  
  const activeButtonStyle: React.CSSProperties = {
    backgroundColor: 'var(--accent-primary)',
    borderColor: 'var(--accent-primary)',
    color: 'white',
  };
  
  // Grid size options
  const gridSizeOptions = [10, 20, 40];
  
  return (
    <div className="absolute top-2 left-2 z-10 flex gap-2">
      {/* Layout Direction Toggle - applies auto-layout */}
      <button
        onClick={toggleDirection}
        className="px-3 py-1.5 border rounded text-sm flex items-center gap-2 hover:opacity-80 transition-opacity"
        style={buttonStyle}
        title={`Layout: ${isHorizontal ? 'Horizontal (Left to Right)' : 'Vertical (Top to Bottom)'}\nClick to switch and auto-arrange nodes`}
      >
        <span>{isHorizontal ? '↔' : '↕'}</span>
        {isHorizontal ? 'LR' : 'TB'}
      </button>
      
      {/* Snap to Grid Toggle */}
      <button
        onClick={() => setSnapToGrid(!snapToGrid)}
        className="px-3 py-1.5 border rounded text-sm flex items-center gap-2 hover:opacity-80 transition-opacity"
        style={snapToGrid ? activeButtonStyle : buttonStyle}
        title={`Snap to Grid: ${snapToGrid ? 'On' : 'Off'} (${gridSize}px)\nClick to ${snapToGrid ? 'disable' : 'enable'} grid snapping`}
      >
        <span>{snapToGrid ? '⊞' : '⊟'}</span>
        Snap
      </button>
      
      {/* Grid Size Selector (only shown when snap is enabled) */}
      {snapToGrid && (
        <select
          value={gridSize}
          onChange={(e) => setGridSize(Number(e.target.value))}
          className="px-2 py-1.5 border rounded text-sm"
          style={buttonStyle}
          title="Grid size in pixels"
        >
          {gridSizeOptions.map(size => (
            <option key={size} value={size}>{size}px</option>
          ))}
        </select>
      )}
      
      {/* Data Flow Overlay Toggle (v2.0) */}
      {/* @see Requirements 3.4: Toggle to show/hide data flow overlays */}
      {onToggleDataFlowOverlay && (
        <button
          onClick={onToggleDataFlowOverlay}
          className="px-3 py-1.5 border rounded text-sm flex items-center gap-2 hover:opacity-80 transition-opacity"
          style={showDataFlowOverlay ? activeButtonStyle : buttonStyle}
          title={`Data Flow Overlay: ${showDataFlowOverlay ? 'On' : 'Off'}\nShows state keys flowing between nodes during execution`}
        >
          <span>🔀</span>
          {showDataFlowOverlay ? 'Flow On' : 'Flow Off'}
        </button>
      )}
      
      {/* Fit to View Button */}
      <button
        onClick={onFitView}
        className="px-3 py-1.5 border rounded text-sm flex items-center gap-2 hover:opacity-80 transition-opacity"
        style={buttonStyle}
        title="Fit all nodes in view (Ctrl+0)"
      >
        <span>⊡</span> Fit
      </button>
      
      {/* Minimap Toggle (v2.0) */}
      {/* @see Requirements 8.1, 8.2: Configurable minimap, toggleable via toolbar */}
      {onToggleMinimap && (
        <button
          onClick={onToggleMinimap}
          className="px-3 py-1.5 border rounded text-sm flex items-center gap-2 hover:opacity-80 transition-opacity"
          style={showMinimap ? activeButtonStyle : buttonStyle}
          title={`Minimap: ${showMinimap ? 'On' : 'Off'}\nShows overview of entire workflow`}
        >
          <span>🗺️</span>
          Map
        </button>
      )}
      
      {/* Zoom Level Display */}
      <div
        className="px-3 py-1.5 border rounded text-sm flex items-center gap-1 cursor-default"
        style={buttonStyle}
        title="Current zoom level (Ctrl+Plus/Minus to zoom)"
      >
        <span>🔍</span>
        {zoomPercent}%
      </div>
      
      {/* Separator */}
      <div className="w-px h-6 self-center" style={{ backgroundColor: 'var(--border-default)' }} />
      
      {/* Run/Stop Buttons (v2.0) */}
      {/* @see Requirements 10.8, 10.9: Run and Stop buttons */}
      {/* Run is disabled when not built or during build */}
      {onRun && !isRunning && (
        <Tooltip 
          content={isBuilding ? 'Build in progress...' : !isBuilt ? 'Build the project first to run' : 'Run the workflow (F5)'}
          position="bottom"
          delay={100}
        >
          <button
            onClick={onRun}
            disabled={!isBuilt || isBuilding}
            className={`px-3 py-1.5 border rounded text-sm flex items-center gap-2 transition-opacity ${
              !isBuilt || isBuilding 
                ? 'opacity-50 cursor-not-allowed' 
                : 'hover:opacity-80'
            }`}
            style={{ 
              backgroundColor: !isBuilt || isBuilding ? 'var(--bg-secondary)' : 'var(--accent-success)', 
              borderColor: !isBuilt || isBuilding ? 'var(--border-default)' : 'var(--accent-success)', 
              color: !isBuilt || isBuilding ? 'var(--text-muted)' : 'white' 
            }}
          >
            <span>{isBuilding ? '⏳' : '▶️'}</span> {isBuilding ? 'Building...' : 'Run'}
          </button>
        </Tooltip>
      )}
      {onStop && isRunning && (
        <button
          onClick={onStop}
          className="px-3 py-1.5 border rounded text-sm flex items-center gap-2 hover:opacity-80 transition-opacity"
          style={{ 
            backgroundColor: 'var(--accent-error)', 
            borderColor: 'var(--accent-error)', 
            color: 'white' 
          }}
          title="Stop execution"
        >
          <span>⏹️</span> Stop
        </button>
      )}
    </div>
  );
}
