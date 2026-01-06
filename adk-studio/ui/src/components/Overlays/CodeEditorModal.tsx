import Editor from '@monaco-editor/react';
import type { FunctionToolConfig } from '../../types/project';
import { generateFunctionTemplate, extractUserCode } from '../../utils/functionTemplates';

interface Props {
  config: FunctionToolConfig;
  onUpdate: (config: FunctionToolConfig) => void;
  onClose: () => void;
}

export function CodeEditorModal({ config, onUpdate, onClose }: Props) {
  return (
    <div className="fixed inset-0 bg-black/80 flex items-center justify-center z-50" onClick={onClose}>
      <div className="bg-studio-panel rounded-lg w-11/12 h-5/6 flex flex-col" onClick={e => e.stopPropagation()}>
        <div className="flex justify-between items-center p-4 border-b border-gray-700">
          <h2 className="text-lg font-semibold text-blue-400">{config.name || 'function'}_fn</h2>
          <button onClick={onClose} className="text-gray-400 hover:text-white text-xl">×</button>
        </div>
        <div className="flex-1 overflow-hidden">
          <Editor
            height="100%"
            defaultLanguage="rust"
            theme="vs-dark"
            value={generateFunctionTemplate(config)}
            onChange={(value) => value && onUpdate({ ...config, code: extractUserCode(value, config) })}
            options={{ minimap: { enabled: false }, fontSize: 14, scrollBeyondLastLine: false, automaticLayout: true, tabSize: 4 }}
          />
        </div>
        <div className="p-4 border-t border-gray-700 flex justify-end">
          <button onClick={onClose} className="px-4 py-2 bg-blue-600 hover:bg-blue-500 rounded text-sm">Done</button>
        </div>
      </div>
    </div>
  );
}
