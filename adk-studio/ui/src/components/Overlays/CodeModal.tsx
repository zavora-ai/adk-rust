import Editor from '@monaco-editor/react';
import type { GeneratedProject } from '../../api/client';

interface Props {
  code: GeneratedProject;
  onClose: () => void;
}

export function CodeModal({ code, onClose }: Props) {
  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-studio-panel rounded-lg w-4/5 h-4/5 flex flex-col" onClick={e => e.stopPropagation()}>
        <div className="flex justify-between items-center p-4 border-b border-gray-700">
          <h2 className="text-lg font-semibold">Generated Rust Code</h2>
          <button onClick={onClose} className="text-gray-400 hover:text-white text-xl">×</button>
        </div>
        <div className="flex-1 overflow-auto p-4">
          {code.files.map(file => (
            <div key={file.path} className="mb-6">
              <div className="flex justify-between items-center mb-2">
                <h3 className="text-sm font-mono text-blue-400">{file.path}</h3>
                <button onClick={() => navigator.clipboard.writeText(file.content)} className="text-xs bg-gray-700 px-2 py-1 rounded hover:bg-gray-600">Copy</button>
              </div>
              <div className="border border-gray-700 rounded overflow-hidden">
                <Editor
                  height={Math.min(600, file.content.split('\n').length * 19 + 20)}
                  language={file.path.endsWith('.toml') ? 'toml' : 'rust'}
                  value={file.content}
                  theme="vs-dark"
                  options={{ readOnly: true, minimap: { enabled: false }, scrollBeyondLastLine: false, fontSize: 12 }}
                />
              </div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
