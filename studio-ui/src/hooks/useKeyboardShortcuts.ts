import { useEffect } from 'react';

interface Props {
  selectedToolId: string | null;
  selectedNodeId: string | null;
  onDeleteTool: (nodeId: string, toolType: string) => void;
  onClearTool: () => void;
}

export function useKeyboardShortcuts({ selectedToolId, selectedNodeId, onDeleteTool, onClearTool }: Props) {
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.key === 'Delete' || e.key === 'Backspace') && selectedToolId && selectedNodeId) {
        const active = document.activeElement;
        if (active?.tagName === 'INPUT' || active?.tagName === 'TEXTAREA') return;
        const parts = selectedToolId.split('_');
        const toolType = parts.slice(-2).join('_');
        onDeleteTool(selectedNodeId, toolType);
        onClearTool();
        e.preventDefault();
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [selectedToolId, selectedNodeId, onDeleteTool, onClearTool]);
}
