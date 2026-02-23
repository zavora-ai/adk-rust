import { useState, useRef, useEffect } from 'react';
import { useStore } from '../../store';
import { TEMPLATES, Template } from './templates';
import { useTheme } from '../../hooks/useTheme';
import { useWalkthrough } from '../../hooks/useWalkthrough';
import { useTemplateWalkthrough } from '../../hooks/useTemplateWalkthrough';
import { KEYBOARD_SHORTCUTS } from '../../hooks/useKeyboardShortcuts';
import { TemplateGallery } from '../Templates';
import type { Template as NewTemplate } from '../Templates/templates';

export type BuildStatusType = 'none' | 'building' | 'success' | 'error';

interface MenuBarProps {
  onExportCode: () => void;
  onNewProject: () => void;
  onTemplateApplied?: () => void;
  /** Callback when Run is requested after template load */
  onRunTemplate?: () => void;
  /** Current build status */
  buildStatus?: BuildStatusType;
  /** Callback when build status indicator is clicked */
  onBuildStatusClick?: () => void;
  /** Whether debug mode is enabled */
  debugMode?: boolean;
  /** Callback when debug mode is toggled */
  onDebugModeToggle?: () => void;
}

export function MenuBar({ onExportCode, onNewProject, onTemplateApplied, onRunTemplate, buildStatus = 'none', onBuildStatusClick, debugMode = false, onDebugModeToggle }: MenuBarProps) {
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const [showTemplateGallery, setShowTemplateGallery] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const { currentProject, closeProject, addAgent, removeAgent, addEdge, removeEdge } = useStore();
  const { mode } = useTheme();
  const { completed: walkthroughCompleted, show: showWalkthrough, reset: resetWalkthrough } = useWalkthrough();
  const { start: startTemplateWalkthrough } = useTemplateWalkthrough();
  const isLight = mode === 'light';

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setOpenMenu(null);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  const applyTemplate = (template: Template) => {
    if (!currentProject) return;

    // Clear existing edges
    (currentProject.workflow?.edges || []).forEach(e => removeEdge(e.from, e.to));

    // Clear existing agents
    Object.keys(currentProject.agents).forEach(id => removeAgent(id));

    // Add all agents from template
    Object.entries(template.agents).forEach(([id, agent]) => {
      addAgent(id, agent);
    });

    // Add edges from template (including port information for multi-port nodes)
    template.edges.forEach(e => addEdge(e.from, e.to, e.fromPort, e.toPort));

    if (onTemplateApplied) {
      onTemplateApplied();
    }

    setOpenMenu(null);
  };

  /**
   * Apply template from the new TemplateGallery component
   * Supports both old and new template formats including action nodes
   */
  const applyNewTemplate = (template: NewTemplate) => {
    if (!currentProject) return;

    // Clear existing edges
    (currentProject.workflow?.edges || []).forEach(e => removeEdge(e.from, e.to));

    // Clear existing agents
    Object.keys(currentProject.agents).forEach(id => removeAgent(id));

    // Clear existing action nodes
    const { removeActionNode, addActionNode } = useStore.getState();
    Object.keys(currentProject.actionNodes || {}).forEach(id => removeActionNode(id));

    // Add all agents from template with proper positions
    const agentEntries = Object.entries(template.agents);
    agentEntries.forEach(([id, agent], index) => {
      // Calculate position if not provided
      const position = agent.position || {
        x: 250 + (index % 3) * 300,
        y: 150 + Math.floor(index / 3) * 200,
      };
      addAgent(id, { ...agent, position });
    });

    // Add all action nodes from template with proper positions
    if (template.actionNodes) {
      const actionEntries = Object.entries(template.actionNodes);
      actionEntries.forEach(([id, node], index) => {
        // Calculate position based on index - arrange in a grid
        // Action nodes go below agents
        const baseY = agentEntries.length > 0 ? 400 : 150;
        const position = {
          x: 100 + (index % 4) * 250,
          y: baseY + Math.floor(index / 4) * 150,
        };
        addActionNode(id, { ...node, position } as any);
      });
    }

    // Add edges from template (including port information for multi-port nodes)
    template.edges.forEach(e => addEdge(e.from, e.to, e.fromPort, e.toPort));

    // Close gallery and apply layout
    setShowTemplateGallery(false);
    
    if (onTemplateApplied) {
      onTemplateApplied();
    }
    
    // Start template walkthrough for automation templates (those with action nodes or env vars)
    if (template.actionNodes || template.envVars || template.useCase) {
      // Small delay to let the canvas render first
      setTimeout(() => {
        startTemplateWalkthrough(template);
      }, 300);
    }
  };

  /**
   * Apply template and immediately run it
   * Requirements: 6.8
   */
  const applyAndRunTemplate = (template: NewTemplate) => {
    applyNewTemplate(template);
    
    // Trigger run after template is applied
    if (onRunTemplate) {
      // Small delay to ensure template is fully loaded
      setTimeout(() => onRunTemplate(), 200);
    }
  };

  const menuButtonClass = isLight
    ? 'hover:bg-gray-200'
    : 'hover:bg-gray-700';
  
  const menuActiveClass = isLight
    ? 'bg-gray-200'
    : 'bg-gray-700';

  const Menu = ({ name, children }: { name: string; children: React.ReactNode }) => (
    <div className="relative">
      <button
        className={`px-3 py-1 text-sm rounded ${menuButtonClass} ${openMenu === name ? menuActiveClass : ''}`}
        style={{ color: 'var(--text-primary)' }}
        onClick={() => setOpenMenu(openMenu === name ? null : name)}
      >
        {name}
      </button>
      {openMenu === name && (
        <div 
          className="absolute top-full left-0 mt-1 rounded shadow-lg min-w-[200px] z-50"
          style={{ backgroundColor: 'var(--surface-panel)', border: '1px solid var(--border-default)' }}
        >
          {children}
        </div>
      )}
    </div>
  );

  const MenuItem = ({ onClick, children, disabled }: { onClick: () => void; children: React.ReactNode; disabled?: boolean }) => (
    <button
      className={`w-full text-left px-3 py-2 text-sm ${menuButtonClass} ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`}
      style={{ color: 'var(--text-primary)' }}
      onClick={() => { if (!disabled) { onClick(); setOpenMenu(null); } }}
      disabled={disabled}
    >
      {children}
    </button>
  );

  const Divider = () => <div className="my-1" style={{ borderTop: '1px solid var(--border-default)' }} />;

  return (
    <div 
      ref={menuRef} 
      className="flex items-center gap-1 px-2 py-1"
      style={{ backgroundColor: 'var(--surface-panel)', borderBottom: '1px solid var(--border-default)' }}
    >
      <span className="text-sm font-semibold mr-4 flex items-center gap-1.5" style={{ color: 'var(--accent-primary)' }}>
        <img src="https://adk-rust.com/icon.svg" alt="ADK" className="w-5 h-5" />
        ADK Studio
      </span>

      <Menu name="File">
        <MenuItem onClick={onNewProject}>📄 New Project</MenuItem>
        {currentProject && (
          <MenuItem onClick={() => closeProject()}>📂 Open Project...</MenuItem>
        )}
        <Divider />
        <MenuItem onClick={onExportCode} disabled={!currentProject}>📦 Export Code</MenuItem>
      </Menu>

      <Menu name="Templates">
        <MenuItem onClick={() => { setShowTemplateGallery(true); setOpenMenu(null); }} disabled={!currentProject}>
          🖼️ Browse Gallery...
        </MenuItem>
        <Divider />
        <div className="px-3 py-1 text-xs" style={{ color: 'var(--text-muted)', borderBottom: '1px solid var(--border-default)' }}>Quick templates</div>
        {TEMPLATES.slice(0, 5).map(t => (
          <MenuItem key={t.id} onClick={() => applyTemplate(t)} disabled={!currentProject}>
            {t.icon} {t.name}
          </MenuItem>
        ))}
      </Menu>

      <Menu name="Help">
        <MenuItem onClick={() => window.open('https://github.com/zavora-ai/adk-rust', '_blank')}>📚 Documentation</MenuItem>
        <Divider />
        <MenuItem onClick={() => { showWalkthrough(); setOpenMenu(null); }}>
          🎓 {walkthroughCompleted ? 'Restart Tutorial' : 'Start Tutorial'}
        </MenuItem>
        {walkthroughCompleted && (
          <MenuItem onClick={() => { resetWalkthrough(); setOpenMenu(null); }}>
            🔄 Reset Tutorial Progress
          </MenuItem>
        )}
        <Divider />
        {/* Keyboard Shortcuts Reference - Requirement 11.9 */}
        <div className="px-3 py-2 text-xs" style={{ color: 'var(--text-secondary)' }}>
          <div className="font-semibold mb-2">⌨️ Keyboard Shortcuts</div>
          {/* Group shortcuts by category */}
          {['Edit', 'Canvas'].map(category => (
            <div key={category} className="mb-2">
              <div className="font-medium mb-1" style={{ color: 'var(--text-muted)' }}>{category}</div>
              {KEYBOARD_SHORTCUTS.filter(s => s.category === category).map(shortcut => (
                <div key={shortcut.key} className="flex justify-between gap-4 py-0.5">
                  <span style={{ color: 'var(--text-muted)' }}>{shortcut.description}</span>
                  <span className="font-mono text-xs px-1 rounded" style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)' }}>
                    {shortcut.key}
                  </span>
                </div>
              ))}
            </div>
          ))}
        </div>
        <Divider />
        <div className="px-3 py-2 text-xs" style={{ color: 'var(--text-muted)' }}>ADK Studio v2.0.0</div>
      </Menu>

      <div className="flex-1" />

      {currentProject && (
        <div className="flex items-center gap-3">
          <span className="text-sm" style={{ color: 'var(--text-secondary)' }}>
            Project: <span style={{ color: 'var(--text-primary)' }}>{currentProject.name}</span>
          </span>
          {/* Build Status Indicator */}
          <button
            onClick={onBuildStatusClick}
            className="flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors"
            style={{
              backgroundColor: buildStatus === 'building' ? 'var(--accent-primary)' 
                : buildStatus === 'success' ? '#22c55e'
                : buildStatus === 'error' ? '#ef4444'
                : 'var(--bg-secondary)',
              color: buildStatus === 'none' ? 'var(--text-muted)' : 'white',
              cursor: buildStatus === 'none' ? 'default' : 'pointer',
            }}
            title={
              buildStatus === 'building' ? 'Building... Click to view progress'
                : buildStatus === 'success' ? 'Build succeeded'
                : buildStatus === 'error' ? 'Build failed - Click to view errors'
                : 'No build yet'
            }
          >
            {buildStatus === 'building' && (
              <>
                <span className="animate-spin">⏳</span>
                <span>Building...</span>
              </>
            )}
            {buildStatus === 'success' && (
              <>
                <span>✓</span>
                <span>Built</span>
              </>
            )}
            {buildStatus === 'error' && (
              <>
                <span>✗</span>
                <span>Failed</span>
              </>
            )}
            {buildStatus === 'none' && (
              <>
                <span>○</span>
                <span>Not Built</span>
              </>
            )}
          </button>
          
          {/* Debug Mode Toggle */}
          <button
            onClick={onDebugModeToggle}
            className="flex items-center gap-1.5 px-2 py-0.5 rounded text-xs font-medium transition-colors"
            style={{
              backgroundColor: debugMode ? '#8b5cf6' : 'var(--bg-secondary)',
              color: debugMode ? 'white' : 'var(--text-muted)',
              border: debugMode ? 'none' : '1px solid var(--border-default)',
            }}
            title={debugMode ? 'Debug mode ON - showing state inspector and timeline' : 'Debug mode OFF - click to show state inspector and timeline'}
          >
            <span>🐛</span>
            <span>{debugMode ? 'Debug' : 'Debug'}</span>
          </button>
        </div>
      )}

      {/* Template Gallery Modal */}
      {showTemplateGallery && (
        <TemplateGallery
          isModal
          onSelect={applyNewTemplate}
          onRun={applyAndRunTemplate}
          onClose={() => setShowTemplateGallery(false)}
        />
      )}
    </div>
  );
}
