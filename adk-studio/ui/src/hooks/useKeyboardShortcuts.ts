import { useEffect } from 'react';

interface Props {
  selectedNodeId: string | null;
  selectedToolId: string | null;
  onDeleteNode: (id: string) => void;
  onDeleteTool: (nodeId: string, toolType: string) => void;
  onDuplicateNode?: (id: string) => string | null;
  onSelectNode: (id: string | null) => void;
  onSelectTool: (id: string | null) => void;
  onAutoLayout?: () => void;
  onFitView?: () => void;
}

export function useKeyboardShortcuts({ selectedNodeId, selectedToolId, onDeleteNode, onDeleteTool, onDuplicateNode, onSelectNode, onSelectTool, onAutoLayout, onFitView }: Props) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const active = document.activeElement;
      if (active?.tagName === 'INPUT' || active?.tagName === 'TEXTAREA') return;

      const isMod = e.metaKey || e.ctrlKey;

      // Delete: remove selected tool or node
      if (e.key === 'Delete' || e.key === 'Backspace') {
        if (selectedToolId && selectedNodeId) {
          const parts = selectedToolId.split('_');
          onDeleteTool(selectedNodeId, parts.slice(-2).join('_'));
          onSelectTool(null);
        } else if (selectedNodeId && selectedNodeId !== 'START' && selectedNodeId !== 'END') {
          onDeleteNode(selectedNodeId);
          onSelectNode(null);
        }
        e.preventDefault();
        return;
      }

      // Ctrl+D: Duplicate
      if (isMod && e.key === 'd' && selectedNodeId && onDuplicateNode) {
        e.preventDefault();
        const newId = onDuplicateNode(selectedNodeId);
        if (newId) onSelectNode(newId);
        return;
      }

      // Ctrl+L: Auto layout
      if (isMod && e.key === 'l' && onAutoLayout) {
        e.preventDefault();
        onAutoLayout();
        return;
      }

      // Ctrl+0: Fit view
      if (isMod && e.key === '0' && onFitView) {
        e.preventDefault();
        onFitView();
        return;
      }

      // Escape: Deselect
      if (e.key === 'Escape') {
        onSelectNode(null);
        onSelectTool(null);
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [selectedNodeId, selectedToolId, onDeleteNode, onDeleteTool, onDuplicateNode, onSelectNode, onSelectTool, onAutoLayout, onFitView]);
}
