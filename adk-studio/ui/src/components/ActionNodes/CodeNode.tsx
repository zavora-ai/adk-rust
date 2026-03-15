/**
 * CodeNode Component for ADK Studio
 * 
 * ReactFlow node wrapper for Code action nodes.
 * Displays language badge, sandbox status, and code preview.
 * 
 * Requirements: 10.1, 10.2, 12.1, 12.3
 */

import { memo } from 'react';
import { ActionNodeBase } from './ActionNodeBase';
import type { CodeNodeConfig, CodeLanguage, SandboxConfig } from '../../types/actionNodes';
import '../../styles/codeNode.css';

interface CodeNodeData extends CodeNodeConfig {
  isActive?: boolean;
}

interface Props {
  data: CodeNodeData;
  selected?: boolean;
}

/**
 * Language display configuration.
 * Rust is the primary code authoring mode. JavaScript and TypeScript
 * are available as secondary scripting / transform support.
 * @see Requirement 10.1
 */
const LANGUAGE_CONFIG: Record<CodeLanguage, { label: string; icon: string; color: string }> = {
  rust: { label: 'Rust', icon: '🦀', color: '#DEA584' },
  javascript: { label: 'JS (Script)', icon: '📜', color: '#F7DF1E' },
  typescript: { label: 'TS (Script)', icon: '📘', color: '#3178C6' },
};

/**
 * Truncates code for preview display.
 */
function getCodePreview(code: string, maxLength: number = 30): string {
  if (!code) return '';
  
  // Get first non-empty line
  const lines = code.split('\n').filter(line => line.trim());
  if (lines.length === 0) return '';
  
  const firstLine = lines[0].trim();
  if (firstLine.length <= maxLength) return firstLine;
  return firstLine.substring(0, maxLength - 3) + '...';
}

/**
 * Counts lines of code.
 */
function getLineCount(code: string): number {
  if (!code) return 0;
  return code.split('\n').filter(line => line.trim()).length;
}

/**
 * Determines sandbox security level.
 * @see Requirement 10.2
 */
function getSandboxLevel(sandbox: SandboxConfig): 'strict' | 'relaxed' | 'open' {
  if (!sandbox.networkAccess && !sandbox.fileSystemAccess) {
    return 'strict';
  }
  if (sandbox.networkAccess && sandbox.fileSystemAccess) {
    return 'open';
  }
  return 'relaxed';
}

/**
 * Gets sandbox indicator configuration.
 */
function getSandboxIndicator(sandbox: SandboxConfig): { icon: string; label: string; className: string } {
  const level = getSandboxLevel(sandbox);
  switch (level) {
    case 'strict':
      return { icon: '🔒', label: 'Strict', className: 'code-sandbox-strict' };
    case 'relaxed':
      return { icon: '🔓', label: 'Relaxed', className: 'code-sandbox-relaxed' };
    case 'open':
      return { icon: '⚠️', label: 'Open', className: 'code-sandbox-open' };
  }
}

/**
 * CodeNode displays Rust-first code execution with secondary JS/TS scripting.
 * 
 * Features:
 * - Language badge with icon (Requirement 10.1)
 * - Sandbox security level indicator (Requirement 10.2)
 * - Code preview with line count
 * - Memory/time limit indicators
 * 
 * Rust is the primary code authoring mode. JavaScript and TypeScript
 * are available as secondary scripting / transform support.
 * 
 * @see Requirements 10.1, 10.2, 12.1, 12.3
 */
export const CodeNode = memo(function CodeNode({ data, selected }: Props) {
  const langConfig = LANGUAGE_CONFIG[data.language] || LANGUAGE_CONFIG.rust;
  const sandboxIndicator = getSandboxIndicator(data.sandbox);
  const lineCount = getLineCount(data.code);
  const codePreview = getCodePreview(data.code);
  
  // Check for resource limits
  const hasLimits = data.sandbox.memoryLimit > 0 || data.sandbox.timeLimit > 0;
  
  return (
    <ActionNodeBase
      type="code"
      label={data.name || 'Code'}
      isActive={data.isActive}
      isSelected={selected}
      status={data.isActive ? 'running' : 'idle'}
    >
      <div className="code-node-content">
        {/* Language and sandbox badges row */}
        <div className="code-node-header-row">
          {/* Language badge */}
          <span 
            className="code-node-language-badge"
            style={{ backgroundColor: langConfig.color }}
            title={`Language: ${data.language}`}
          >
            <span className="code-node-language-icon">{langConfig.icon}</span>
            <span className="code-node-language-label">{langConfig.label}</span>
          </span>
          
          {/* Sandbox indicator */}
          <span 
            className={`code-node-sandbox-badge ${sandboxIndicator.className}`}
            title={`Sandbox: ${sandboxIndicator.label}`}
          >
            <span className="code-node-sandbox-icon">{sandboxIndicator.icon}</span>
            <span className="code-node-sandbox-label">{sandboxIndicator.label}</span>
          </span>
          
          {/* Line count */}
          {lineCount > 0 && (
            <span className="code-node-lines-badge" title={`${lineCount} lines of code`}>
              {lineCount} {lineCount === 1 ? 'line' : 'lines'}
            </span>
          )}
        </div>
        
        {/* Code preview */}
        {codePreview && (
          <div className="code-node-preview">
            <code className="code-node-preview-code">{codePreview}</code>
          </div>
        )}
        
        {/* Resource limits indicators */}
        {hasLimits && (
          <div className="code-node-limits">
            {data.sandbox.memoryLimit > 0 && (
              <span className="code-node-limit-badge" title={`Memory limit: ${data.sandbox.memoryLimit}MB`}>
                💾 {data.sandbox.memoryLimit}MB
              </span>
            )}
            {data.sandbox.timeLimit > 0 && (
              <span className="code-node-limit-badge" title={`Time limit: ${data.sandbox.timeLimit}ms`}>
                ⏱️ {data.sandbox.timeLimit}ms
              </span>
            )}
          </div>
        )}
        
        {/* Access indicators */}
        <div className="code-node-access">
          {data.sandbox.networkAccess && (
            <span className="code-node-access-badge code-node-access-network" title="Network access enabled">
              🌐
            </span>
          )}
          {data.sandbox.fileSystemAccess && (
            <span className="code-node-access-badge code-node-access-fs" title="File system access enabled">
              📁
            </span>
          )}
        </div>
      </div>
    </ActionNodeBase>
  );
});

export default CodeNode;
