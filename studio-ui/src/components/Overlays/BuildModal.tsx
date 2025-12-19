import { useRef, useEffect } from 'react';

interface Props {
  building: boolean;
  success: boolean;
  output: string;
  path: string | null;
  onClose: () => void;
}

export function BuildModal({ building, success, output, path, onClose }: Props) {
  const preRef = useRef<HTMLPreElement>(null);
  
  useEffect(() => {
    if (preRef.current && building) preRef.current.scrollTop = preRef.current.scrollHeight;
  }, [output, building]);

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-studio-panel rounded-lg w-3/5 max-h-4/5 flex flex-col" onClick={e => e.stopPropagation()}>
        <div className="flex justify-between items-center p-4 border-b border-gray-700">
          <h2 className={`text-lg font-semibold ${building ? 'text-blue-400' : success ? 'text-green-400' : 'text-red-400'}`}>
            {building ? '⏳ Building...' : success ? '✓ Build Successful' : '✗ Build Failed'}
          </h2>
          <button onClick={onClose} className="text-gray-400 hover:text-white text-xl">×</button>
        </div>
        <div className="flex-1 overflow-auto p-4">
          {path && (
            <div className="mb-4 p-3 bg-green-900/30 rounded">
              <div className="text-sm text-gray-400">Binary path:</div>
              <code className="text-green-400 text-sm">{path}</code>
            </div>
          )}
          <pre ref={preRef} className="bg-gray-900 p-4 rounded text-xs overflow-auto whitespace-pre max-h-96">{output}</pre>
        </div>
      </div>
    </div>
  );
}
