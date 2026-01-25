import React from 'react';
import type { AgentSchema, ToolConfig } from '../../types/project';
import { TOOL_TYPES } from './ToolPalette';
import { ModelSelector } from './ModelSelector';

interface Props {
  nodeId: string;
  agent: AgentSchema;
  agents: Record<string, AgentSchema>;
  toolConfigs: Record<string, ToolConfig>;
  onUpdate: (id: string, updates: Partial<AgentSchema>) => void;
  onRename: (oldId: string, newId: string) => void;
  onAddSubAgent: () => void;
  onClose: () => void;
  onSelectTool: (toolId: string) => void;
  onRemoveTool: (toolType: string) => void;
  onAddTool?: (agentId: string, toolType: string) => void;
}

export function PropertiesPanel({ nodeId, agent, agents, toolConfigs, onUpdate, onRename, onAddSubAgent, onClose, onSelectTool, onRemoveTool }: Props) {
  const isContainer = agent.type === 'sequential' || agent.type === 'loop' || agent.type === 'parallel';
  const [editingName, setEditingName] = React.useState(false);
  const [newName, setNewName] = React.useState(nodeId);

  const handleRename = () => {
    const trimmed = newName.trim().replace(/\s+/g, '_');
    if (trimmed && trimmed !== nodeId && !agents[trimmed]) {
      onRename(nodeId, trimmed);
    } else {
      setNewName(nodeId);
    }
    setEditingName(false);
  };

  const handleRemoveToolFromSubAgent = (agentId: string, toolType: string) => {
    const subAgent = agents[agentId];
    if (subAgent) {
      onUpdate(agentId, { tools: subAgent.tools.filter(t => t !== toolType) });
    }
  };

  const handleAddToolToSubAgent = (agentId: string, toolType: string) => {
    const subAgent = agents[agentId];
    if (subAgent) {
      let toolId = toolType;
      if (toolType === 'function' || toolType === 'mcp') {
        const existing = subAgent.tools.filter(t => t.startsWith(toolType));
        toolId = `${toolType}_${existing.length + 1}`;
      } else if (subAgent.tools.includes(toolType)) {
        return;
      }
      onUpdate(agentId, { tools: [...subAgent.tools, toolId] });
    }
  };

  return (
    <div 
      className="w-72 border-l p-4 overflow-y-auto"
      style={{ backgroundColor: 'var(--surface-panel)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
    >
      <div className="flex justify-between items-center mb-4">
        {editingName ? (
          <input
            autoFocus
            className="flex-1 px-2 py-1 border border-blue-500 rounded text-sm font-semibold"
            style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)' }}
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onBlur={handleRename}
            onKeyDown={(e) => e.key === 'Enter' && handleRename()}
          />
        ) : (
          <h3 
            className="font-semibold cursor-pointer hover:text-blue-500" 
            style={{ color: 'var(--text-primary)' }}
            onClick={() => setEditingName(true)} 
            title="Click to rename"
          >
            {nodeId} <span style={{ color: 'var(--text-muted)' }} className="text-xs">✎</span>
          </h3>
        )}
        <button 
          onClick={onClose} 
          className="px-2 py-1 rounded text-xs ml-2"
          style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
        >
          Close
        </button>
      </div>

      {isContainer ? (
        <ContainerProperties nodeId={nodeId} agent={agent} agents={agents} onUpdate={onUpdate} onAddSubAgent={onAddSubAgent} onSelectTool={onSelectTool} onRemoveTool={handleRemoveToolFromSubAgent} onAddTool={handleAddToolToSubAgent} />
      ) : agent.type === 'router' ? (
        <RouterProperties nodeId={nodeId} agent={agent} onUpdate={onUpdate} />
      ) : (
        <LlmProperties nodeId={nodeId} agent={agent} toolConfigs={toolConfigs} onUpdate={onUpdate} onSelectTool={onSelectTool} onRemoveTool={onRemoveTool} />
      )}
    </div>
  );
}

function ContainerProperties({ nodeId, agent, agents, onUpdate, onAddSubAgent, onSelectTool, onRemoveTool, onAddTool }: { nodeId: string; agent: AgentSchema; agents: Record<string, AgentSchema>; onUpdate: Props['onUpdate']; onAddSubAgent: () => void; onSelectTool: (id: string) => void; onRemoveTool: (agentId: string, toolType: string) => void; onAddTool: (agentId: string, toolType: string) => void }) {
  const [selectedSubAgent, setSelectedSubAgent] = React.useState<string | null>(null);
  
  return (
    <div>
      {agent.type === 'loop' && (
        <>
          <div 
            className="mb-4 p-2 rounded text-xs"
            style={{ backgroundColor: 'rgba(128, 90, 213, 0.15)', border: '1px solid var(--node-loop)', color: 'var(--text-primary)' }}
          >
            <div className="font-semibold mb-1" style={{ color: 'var(--node-loop)' }}>💡 Loop Agent Tips</div>
            <p style={{ color: 'var(--text-secondary)' }}>Sub-agents run repeatedly until max iterations or exit_loop tool is called.</p>
          </div>
          <div className="mb-4">
            <label className="block text-sm mb-1" style={{ color: 'var(--text-secondary)' }}>Max Iterations</label>
            <input
              type="number"
              min="1"
              className="w-full px-2 py-1 border rounded text-sm"
              style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
              value={agent.max_iterations || 3}
              onChange={(e) => onUpdate(nodeId, { max_iterations: parseInt(e.target.value) || 3 })}
            />
          </div>
        </>
      )}
      <label className="block text-sm mb-2" style={{ color: 'var(--text-secondary)' }}>
        Sub-Agents {agent.type === 'parallel' ? '(run concurrently)' : '(in order)'}
      </label>
      {(agent.sub_agents || []).map((subId, idx) => {
        const subAgent = agents[subId];
        if (!subAgent) return null;
        const isExpanded = selectedSubAgent === subId;
        return (
          <div 
            key={subId} 
            className="mb-2 rounded overflow-hidden"
            style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)' }}
          >
            <div 
              className="p-2 cursor-pointer flex items-center justify-between"
              style={{ color: 'var(--text-primary)' }}
              onClick={() => setSelectedSubAgent(isExpanded ? null : subId)}
            >
              <span className="text-sm font-medium">{agent.type === 'parallel' ? '∥' : `${idx + 1}.`} {subId}</span>
              <span style={{ color: 'var(--text-muted)' }} className="text-xs">{isExpanded ? '▼' : '▶'}</span>
            </div>
            {isExpanded && (
              <div className="p-2 pt-0" style={{ borderTop: '1px solid var(--border-default)' }}>
                <label className="block text-xs mb-1" style={{ color: 'var(--text-secondary)' }}>Model</label>
                <div className="mb-2">
                  <ModelSelector
                    value={subAgent.model || ''}
                    onChange={(model) => onUpdate(subId, { model })}
                    compact
                  />
                </div>
                <label className="block text-xs mb-1" style={{ color: 'var(--text-secondary)' }}>Instruction</label>
                <textarea
                  className="w-full px-2 py-1 border rounded text-xs h-16 mb-2"
                  style={{ backgroundColor: 'var(--bg-primary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
                  value={subAgent.instruction}
                  onChange={(e) => onUpdate(subId, { instruction: e.target.value })}
                />
                <label className="block text-xs mb-1" style={{ color: 'var(--text-secondary)' }}>Tools</label>
                <div className="flex flex-wrap gap-1 mb-2">
                  {(subAgent.tools || []).map(t => {
                    const baseType = t.startsWith('function') ? 'function' : t.startsWith('mcp') ? 'mcp' : t;
                    const tool = TOOL_TYPES.find(tt => tt.type === baseType);
                    const toolId = `${subId}_${t}`;
                    return (
                      <span 
                        key={t} 
                        className="text-xs px-2 py-0.5 rounded flex items-center gap-1 cursor-pointer"
                        style={{ backgroundColor: 'var(--bg-primary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
                        onClick={() => onSelectTool(toolId)}
                      >
                        {tool?.icon} {t} <button onClick={(e) => { e.stopPropagation(); onRemoveTool(subId, t); }} style={{ color: 'var(--accent-error)' }}>×</button>
                      </span>
                    );
                  })}
                </div>
                <div className="flex flex-wrap gap-1">
                  {TOOL_TYPES.filter(t => !subAgent.tools?.includes(t.type) || t.type === 'function' || t.type === 'mcp').map(t => (
                    <button 
                      key={t.type} 
                      onClick={() => onAddTool(subId, t.type)} 
                      className="text-xs px-2 py-0.5 rounded"
                      style={{ backgroundColor: 'var(--accent-primary)', color: 'white' }}
                    >
                      + {t.icon}
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
        );
      })}
      <button 
        onClick={onAddSubAgent} 
        className="w-full py-2 rounded text-sm"
        style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
      >
        + Add Sub-Agent
      </button>
    </div>
  );
}

function RouterProperties({ nodeId, agent, onUpdate }: { nodeId: string; agent: AgentSchema; onUpdate: Props['onUpdate'] }) {
  return (
    <div>
      <label className="block text-sm mb-1" style={{ color: 'var(--text-secondary)' }}>Model</label>
      <div className="mb-3">
        <ModelSelector
          value={agent.model || ''}
          onChange={(model) => onUpdate(nodeId, { model })}
        />
      </div>
      <label className="block text-sm mb-1" style={{ color: 'var(--text-secondary)' }}>Routing Instruction</label>
      <textarea
        className="w-full px-2 py-1 border rounded text-sm h-20 mb-3"
        style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
        value={agent.instruction}
        onChange={(e) => onUpdate(nodeId, { instruction: e.target.value })}
      />
      <label className="block text-sm mb-2" style={{ color: 'var(--text-secondary)' }}>Routes</label>
      {(agent.routes || []).map((route, idx) => (
        <div key={idx} className="flex gap-1 mb-2 items-center">
          <input
            className="flex-1 px-2 py-1 border rounded text-xs"
            style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
            placeholder="condition"
            value={route.condition}
            onChange={(e) => {
              const routes = [...(agent.routes || [])];
              routes[idx] = { ...route, condition: e.target.value };
              onUpdate(nodeId, { routes });
            }}
          />
          <span style={{ color: 'var(--text-muted)' }}>→</span>
          <input
            className="flex-1 px-2 py-1 border rounded text-xs"
            style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
            placeholder="target"
            value={route.target}
            onChange={(e) => {
              const routes = [...(agent.routes || [])];
              routes[idx] = { ...route, target: e.target.value };
              onUpdate(nodeId, { routes });
            }}
          />
          <button style={{ color: 'var(--accent-error)' }} className="text-sm" onClick={() => onUpdate(nodeId, { routes: (agent.routes || []).filter((_, i) => i !== idx) })}>×</button>
        </div>
      ))}
      <button 
        className="w-full py-1 rounded text-xs" 
        style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
        onClick={() => onUpdate(nodeId, { routes: [...(agent.routes || []), { condition: '', target: '' }] })}
      >
        + Add Route
      </button>
    </div>
  );
}

function LlmProperties({ nodeId, agent, toolConfigs, onUpdate, onSelectTool, onRemoveTool }: { nodeId: string; agent: AgentSchema; toolConfigs: Record<string, ToolConfig>; onUpdate: Props['onUpdate']; onSelectTool: (id: string) => void; onRemoveTool: (type: string) => void }) {
  const [showAdvanced, setShowAdvanced] = React.useState(false);

  return (
    <div className="space-y-4">
      {/* Basic Settings */}
      <Section title="Basic">
        <Field label="Model">
          <ModelSelector
            value={agent.model || ''}
            onChange={(model) => onUpdate(nodeId, { model })}
          />
        </Field>
        <Field label="Instruction">
          <textarea
            className="w-full px-2 py-1 border rounded text-sm h-24"
            style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
            value={agent.instruction}
            onChange={(e) => onUpdate(nodeId, { instruction: e.target.value })}
            placeholder="You are a helpful assistant..."
          />
        </Field>
        <Field label="Description" hint="Optional agent description">
          <input
            className="w-full px-2 py-1 border rounded text-sm"
            style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
            value={agent.description || ''}
            onChange={(e) => onUpdate(nodeId, { description: e.target.value })}
            placeholder="What this agent does"
          />
        </Field>
      </Section>

      {/* Tools */}
      {agent.tools.length > 0 && (
        <Section title="Tools">
          <div className="flex flex-wrap gap-1">
            {agent.tools.map(t => {
              const baseType = t.startsWith('function') ? 'function' : t.startsWith('mcp') ? 'mcp' : t;
              const tool = TOOL_TYPES.find(tt => tt.type === baseType);
              const toolId = `${nodeId}_${t}`;
              const config = toolConfigs[toolId];
              const displayName = config && 'name' in config && config.name ? config.name : tool?.label || t;
              return (
                <span 
                  key={t} 
                  className="text-xs px-2 py-1 rounded flex items-center gap-1 cursor-pointer hover:opacity-80"
                  style={{ backgroundColor: config ? '#38A169' : 'var(--bg-secondary)', color: config ? 'white' : 'var(--text-primary)', border: '1px solid var(--border-default)' }}
                  onClick={() => onSelectTool(toolId)}
                >
                  {tool?.icon} {displayName} <span className="text-blue-400">⚙</span>
                  <button onClick={(e) => { e.stopPropagation(); onRemoveTool(t); }} className="ml-1 text-red-500 hover:text-red-400">×</button>
                </span>
              );
            })}
          </div>
        </Section>
      )}

      {/* Advanced Settings (collapsible) */}
      <div>
        <button 
          onClick={() => setShowAdvanced(!showAdvanced)}
          className="w-full text-left text-xs flex items-center gap-1"
          style={{ color: 'var(--text-secondary)' }}
        >
          <span>{showAdvanced ? '▼' : '▶'}</span> Advanced Settings
        </button>
        
        {showAdvanced && (
          <div className="mt-2 space-y-3 pl-2 border-l" style={{ borderColor: 'var(--border-default)' }}>
            <Field label="Global Instruction" hint="System-level instruction prepended to all prompts">
              <textarea
                className="w-full px-2 py-1 border rounded text-xs h-16"
                style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
                value={agent.global_instruction || ''}
                onChange={(e) => onUpdate(nodeId, { global_instruction: e.target.value })}
                placeholder="Always respond in JSON format..."
              />
            </Field>
            <Field label="Output Key" hint="Custom state key for agent output">
              <input
                className="w-full px-2 py-1 border rounded text-xs"
                style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
                value={agent.output_key || ''}
                onChange={(e) => onUpdate(nodeId, { output_key: e.target.value })}
                placeholder="response (default)"
              />
            </Field>
            <Field label="Output Schema" hint="JSON Schema for structured output">
              <textarea
                className="w-full px-2 py-1 border rounded text-xs h-20 font-mono"
                style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
                value={agent.output_schema || ''}
                onChange={(e) => onUpdate(nodeId, { output_schema: e.target.value })}
                placeholder='{"type": "object", "properties": {...}}'
              />
            </Field>
            <Field label="Include Contents">
              <select
                className="w-full px-2 py-1 border rounded text-xs"
                style={{ backgroundColor: 'var(--bg-secondary)', borderColor: 'var(--border-default)', color: 'var(--text-primary)' }}
                value={agent.include_contents || 'all'}
                onChange={(e) => onUpdate(nodeId, { include_contents: e.target.value as 'all' | 'none' | 'last' })}
              >
                <option value="all">All history</option>
                <option value="last">Last message only</option>
                <option value="none">None</option>
              </select>
            </Field>
          </div>
        )}
      </div>
    </div>
  );
}

// Helper components
function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-xs font-semibold uppercase tracking-wide mb-2" style={{ color: 'var(--text-muted)' }}>{title}</div>
      <div className="space-y-2">{children}</div>
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="block text-sm mb-1" style={{ color: 'var(--text-secondary)' }}>
        {label}
        {hint && <span className="text-xs ml-1" style={{ color: 'var(--text-muted)' }}>({hint})</span>}
      </label>
      {children}
    </div>
  );
}
