import { useStore } from '../../store';

interface CanvasToolbarProps {
  onAutoLayout: () => void;
  onFitView: () => void;
}

export function CanvasToolbar({ onAutoLayout, onFitView }: CanvasToolbarProps) {
  const layoutDirection = useStore(s => s.layoutDirection);
  const isHorizontal = layoutDirection === 'LR';
  
  return (
    <div className="absolute top-2 left-2 z-10 flex gap-2">
      <button
        onClick={onAutoLayout}
        className="px-3 py-1.5 bg-gray-800 hover:bg-gray-700 border border-gray-600 rounded text-sm flex items-center gap-2 text-gray-200"
        title={`Click to switch to ${isHorizontal ? 'Vertical' : 'Horizontal'} layout`}
      >
        <span>{isHorizontal ? '↔' : '↕'}</span> Layout
      </button>
      <button
        onClick={onFitView}
        className="px-3 py-1.5 bg-gray-800 hover:bg-gray-700 border border-gray-600 rounded text-sm flex items-center gap-2 text-gray-200"
        title="Fit to View"
      >
        <span>⊡</span> Fit
      </button>
    </div>
  );
}
