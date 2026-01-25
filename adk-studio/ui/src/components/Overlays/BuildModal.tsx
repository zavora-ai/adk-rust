import { useRef, useEffect } from 'react';

interface Props {
  building: boolean;
  success: boolean;
  output: string;
  path: string | null;
  onClose: () => void;
  isAutobuild?: boolean;
}

export function BuildModal({ building, success, output, path, onClose, isAutobuild = false }: Props) {
  const preRef = useRef<HTMLPreElement>(null);
  
  useEffect(() => {
    if (preRef.current && building) preRef.current.scrollTop = preRef.current.scrollHeight;
  }, [output, building]);

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={onClose}>
      <div 
        className="rounded-lg w-3/5 max-h-4/5 flex flex-col"
        style={{ backgroundColor: 'var(--surface-panel)' }}
        onClick={e => e.stopPropagation()}
      >
        <div 
          className="flex justify-between items-center p-4 border-b"
          style={{ borderColor: 'var(--border-default)' }}
        >
          <h2 className={`text-lg font-semibold ${building ? 'text-blue-500' : success ? 'text-green-500' : 'text-red-500'}`}>
            {building 
              ? (isAutobuild ? '⚡ Auto Building...' : '⏳ Building...') 
              : success 
                ? '✓ Build Successful' 
                : '✗ Build Failed'}
          </h2>
          <button 
            onClick={onClose} 
            className="text-xl hover:opacity-70"
            style={{ color: 'var(--text-muted)' }}
          >
            ×
          </button>
        </div>
        <div className="flex-1 overflow-auto p-4">
          {path && (
            <div 
              className="mb-4 p-3 rounded border"
              style={{ 
                backgroundColor: 'rgba(56, 161, 105, 0.1)', 
                borderColor: 'var(--accent-success)' 
              }}
            >
              <div className="text-sm" style={{ color: 'var(--text-secondary)' }}>Binary path:</div>
              <code className="text-sm font-mono" style={{ color: 'var(--accent-success)' }}>{path}</code>
            </div>
          )}
          <pre 
            ref={preRef} 
            className="p-4 rounded text-xs overflow-auto whitespace-pre max-h-96"
            style={{ 
              backgroundColor: 'var(--bg-secondary)', 
              color: 'var(--text-primary)',
              border: '1px solid var(--border-default)'
            }}
          >
            {output}
          </pre>
        </div>
      </div>
    </div>
  );
}
