import { useEffect } from 'react';

/**
 * Keyboard shortcuts configuration for ADK Studio
 * @see Requirements 11.1-11.10: Keyboard Shortcuts
 */
interface Props {
  selectedNodeId: string | null;
  selectedToolId: string | null;
  /** v2.0: Selected action node ID */
  selectedActionNodeId?: string | null;
  onDeleteNode: (id: string) => void;
  /** v2.0: Delete action node handler */
  onDeleteActionNode?: (id: string) => void;
  onDeleteTool: (nodeId: string, toolType: string) => void;
  onDuplicateNode?: (id: string) => string | null;
  onSelectNode: (id: string | null) => void;
  /** v2.0: Select action node handler */
  onSelectActionNode?: (id: string | null) => void;
  onSelectTool: (id: string | null) => void;
  onAutoLayout?: () => void;
  onFitView?: () => void;
  /** v2.0: Zoom in handler (Ctrl/Cmd + Plus) */
  onZoomIn?: () => void;
  /** v2.0: Zoom out handler (Ctrl/Cmd + Minus) */
  onZoomOut?: () => void;
  /** v2.0: Undo handler (Ctrl/Cmd + Z) - for future use */
  onUndo?: () => void;
  /** v2.0: Redo handler (Ctrl/Cmd + Shift + Z) - for future use */
  onRedo?: () => void;
  /** Run workflow handler (F5) */
  onRun?: () => void;
}

/**
 * Keyboard shortcuts reference for Help menu
 * @see Requirements 11.9: Display keyboard shortcuts in Help menu
 */
export const KEYBOARD_SHORTCUTS = [
  { key: 'F5', description: 'Run workflow', category: 'Execution' },
  { key: 'Delete / Backspace', description: 'Delete selected node or tool', category: 'Edit' },
  { key: 'Ctrl/Cmd + D', description: 'Duplicate selected node', category: 'Edit' },
  { key: 'Escape', description: 'Deselect all', category: 'Edit' },
  { key: 'Ctrl/Cmd + Z', description: 'Undo', category: 'Edit' },
  { key: 'Ctrl/Cmd + Shift + Z', description: 'Redo', category: 'Edit' },
  { key: 'Ctrl/Cmd + L', description: 'Apply auto-layout', category: 'Canvas' },
  { key: 'Ctrl/Cmd + 0', description: 'Fit all nodes in view', category: 'Canvas' },
  { key: 'Ctrl/Cmd + Plus', description: 'Zoom in', category: 'Canvas' },
  { key: 'Ctrl/Cmd + Minus', description: 'Zoom out', category: 'Canvas' },
] as const;

/**
 * Hook for handling keyboard shortcuts in ADK Studio
 * @see Requirements 11.1-11.10: Keyboard Shortcuts
 * 
 * Implemented shortcuts:
 * - Delete/Backspace: Delete selected node (11.1)
 * - Ctrl+D: Duplicate selected node (11.2)
 * - Ctrl+L: Trigger auto-layout (11.3)
 * - Ctrl+0: Fit-to-view (11.4)
 * - Ctrl+Z: Undo (11.5) - placeholder for Task 24
 * - Ctrl+Shift+Z: Redo (11.6) - placeholder for Task 24
 * - Escape: Deselect all nodes (11.7)
 * - Ctrl+Plus/Minus: Zoom in/out (11.8)
 * - Ignores shortcuts when in input fields (11.10)
 */
export function useKeyboardShortcuts({ 
  selectedNodeId, 
  selectedToolId, 
  selectedActionNodeId,
  onDeleteNode, 
  onDeleteActionNode,
  onDeleteTool, 
  onDuplicateNode, 
  onSelectNode, 
  onSelectActionNode,
  onSelectTool, 
  onAutoLayout, 
  onFitView,
  onZoomIn,
  onZoomOut,
  onUndo,
  onRedo,
  onRun,
}: Props) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const active = document.activeElement;
      
      // F5: Run workflow — works globally, even from input fields
      if (e.key === 'F5' && onRun) {
        e.preventDefault();
        onRun();
        return;
      }

      // Requirement 11.10: Ignore other shortcuts when typing in input fields
      if (active?.tagName === 'INPUT' || active?.tagName === 'TEXTAREA' || 
          (active as HTMLElement)?.isContentEditable) {
        return;
      }

      const isMod = e.metaKey || e.ctrlKey;
      const isShift = e.shiftKey;

      // Requirement 11.1: Delete/Backspace - remove selected tool, action node, or agent node
      if (e.key === 'Delete' || e.key === 'Backspace') {
        if (selectedToolId && selectedNodeId) {
          const parts = selectedToolId.split('_');
          onDeleteTool(selectedNodeId, parts.slice(-2).join('_'));
          onSelectTool(null);
        } else if (selectedActionNodeId && onDeleteActionNode) {
          // Delete selected action node
          onDeleteActionNode(selectedActionNodeId);
          onSelectActionNode?.(null);
        } else if (selectedNodeId && selectedNodeId !== 'START' && selectedNodeId !== 'END') {
          onDeleteNode(selectedNodeId);
          onSelectNode(null);
        }
        e.preventDefault();
        return;
      }

      // Requirement 11.2: Ctrl+D - Duplicate selected node
      if (isMod && e.key === 'd' && selectedNodeId && onDuplicateNode) {
        e.preventDefault();
        const newId = onDuplicateNode(selectedNodeId);
        if (newId) onSelectNode(newId);
        return;
      }

      // Requirement 11.3: Ctrl+L - Auto layout
      if (isMod && e.key === 'l' && onAutoLayout) {
        e.preventDefault();
        onAutoLayout();
        return;
      }

      // Requirement 11.4: Ctrl+0 - Fit view
      if (isMod && e.key === '0' && onFitView) {
        e.preventDefault();
        onFitView();
        return;
      }

      // Requirement 11.5: Ctrl+Z - Undo (placeholder for Task 24)
      if (isMod && !isShift && e.key === 'z' && onUndo) {
        e.preventDefault();
        onUndo();
        return;
      }

      // Requirement 11.6: Ctrl+Shift+Z - Redo (placeholder for Task 24)
      if (isMod && isShift && (e.key === 'z' || e.key === 'Z') && onRedo) {
        e.preventDefault();
        onRedo();
        return;
      }

      // Requirement 11.7: Escape - Deselect all
      if (e.key === 'Escape') {
        onSelectNode(null);
        onSelectTool(null);
        return;
      }

      // Requirement 11.8: Ctrl+Plus - Zoom in
      // Note: '+' key is '=' on most keyboards, and numpad '+' is 'NumpadAdd'
      if (isMod && (e.key === '+' || e.key === '=' || e.key === 'NumpadAdd') && onZoomIn) {
        e.preventDefault();
        onZoomIn();
        return;
      }

      // Requirement 11.8: Ctrl+Minus - Zoom out
      if (isMod && (e.key === '-' || e.key === 'NumpadSubtract') && onZoomOut) {
        e.preventDefault();
        onZoomOut();
        return;
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [
    selectedNodeId, 
    selectedToolId, 
    selectedActionNodeId,
    onDeleteNode, 
    onDeleteActionNode,
    onDeleteTool, 
    onDuplicateNode, 
    onSelectNode, 
    onSelectActionNode,
    onSelectTool, 
    onAutoLayout, 
    onFitView,
    onZoomIn,
    onZoomOut,
    onUndo,
    onRedo,
    onRun,
  ]);
}
