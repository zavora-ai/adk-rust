import React from 'react';
import type { AgentSchema, ToolConfig } from '../../types/project';
import { TOOL_TYPES } from './ToolPalette';

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
    <div className="w-72 bg-studio-panel border-l border-gray-700 p-4 overflow-y-auto">
      <div className="flex justify-between items-center mb-4">
        {editingName ? (
          <input
            autoFocus
            className="flex-1 px-2 py-1 bg-studio-bg border border-blue-500 rounded text-sm font-semibold"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onBlur={handleRename}
            onKeyDown={(e) => e.key === 'Enter' && handleRename()}
          />
        ) : (
          <h3 className="font-semibold cursor-pointer hover:text-blue-400" onClick={() => setEditingName(true)} title="Click to rename">
            {nodeId} <span className="text-xs text-gray-500">✎</span>
          </h3>
        )}
        <button onClick={onClose} className="px-2 py-1 bg-gray-600 rounded text-xs ml-2">Close</button>
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
          <div className="mb-4 p-2 bg-purple-900/50 border border-purple-600 rounded text-xs">
            <div className="font-semibold text-purple-400 mb-1">💡 Loop Agent Tips</div>
            <p className="text-purple-200">Sub-agents run repeatedly until max iterations or exit_loop tool is called.</p>
          </div>
          <div className="mb-4">
            <label className="block text-sm text-gray-400 mb-1">Max Iterations</label>
            <input
              type="number"
              min="1"
              className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm"
              value={agent.max_iterations || 3}
              onChange={(e) => onUpdate(nodeId, { max_iterations: parseInt(e.target.value) || 3 })}
            />
          </div>
        </>
      )}
      <label className="block text-sm text-gray-400 mb-2">
        Sub-Agents {agent.type === 'parallel' ? '(run concurrently)' : '(in order)'}
      </label>
      {(agent.sub_agents || []).map((subId, idx) => {
        const subAgent = agents[subId];
        if (!subAgent) return null;
        const isExpanded = selectedSubAgent === subId;
        return (
          <div key={subId} className="mb-2 bg-gray-800 rounded overflow-hidden">
            <div 
              className="p-2 cursor-pointer hover:bg-gray-700 flex items-center justify-between"
              onClick={() => setSelectedSubAgent(isExpanded ? null : subId)}
            >
              <span className="text-sm font-medium">{agent.type === 'parallel' ? '∥' : `${idx + 1}.`} {subId}</span>
              <span className="text-gray-500 text-xs">{isExpanded ? '▼' : '▶'}</span>
            </div>
            {isExpanded && (
              <div className="p-2 pt-0 border-t border-gray-700">
                <label className="block text-xs text-gray-400 mb-1">Model</label>
                <input
                  className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs mb-2"
                  value={subAgent.model || ''}
                  onChange={(e) => onUpdate(subId, { model: e.target.value })}
                />
                <label className="block text-xs text-gray-400 mb-1">Instruction</label>
                <textarea
                  className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs h-16 mb-2"
                  value={subAgent.instruction}
                  onChange={(e) => onUpdate(subId, { instruction: e.target.value })}
                />
                <label className="block text-xs text-gray-400 mb-1">Tools</label>
                <div className="flex flex-wrap gap-1 mb-2">
                  {(subAgent.tools || []).map(t => {
                    const baseType = t.startsWith('function') ? 'function' : t.startsWith('mcp') ? 'mcp' : t;
                    const tool = TOOL_TYPES.find(tt => tt.type === baseType);
                    const toolId = `${subId}_${t}`;
                    return (
                      <span key={t} className="text-xs px-2 py-0.5 bg-gray-700 rounded flex items-center gap-1 cursor-pointer hover:bg-gray-600" onClick={() => onSelectTool(toolId)}>
                        {tool?.icon} {t} <button onClick={(e) => { e.stopPropagation(); onRemoveTool(subId, t); }} className="text-red-400 hover:text-red-300">×</button>
                      </span>
                    );
                  })}
                </div>
                <div className="flex flex-wrap gap-1">
                  {TOOL_TYPES.filter(t => !subAgent.tools?.includes(t.type) || t.type === 'function' || t.type === 'mcp').map(t => (
                    <button key={t.type} onClick={() => onAddTool(subId, t.type)} className="text-xs px-2 py-0.5 bg-blue-800 hover:bg-blue-700 rounded">
                      + {t.icon}
                    </button>
                  ))}
                </div>
              </div>
            )}
          </div>
        );
      })}
      <button onClick={onAddSubAgent} className="w-full py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm">+ Add Sub-Agent</button>
    </div>
  );
}

function RouterProperties({ nodeId, agent, onUpdate }: { nodeId: string; agent: AgentSchema; onUpdate: Props['onUpdate'] }) {
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1">Model</label>
      <input
        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm mb-3"
        value={agent.model || ''}
        onChange={(e) => onUpdate(nodeId, { model: e.target.value })}
      />
      <label className="block text-sm text-gray-400 mb-1">Routing Instruction</label>
      <textarea
        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-20 mb-3"
        value={agent.instruction}
        onChange={(e) => onUpdate(nodeId, { instruction: e.target.value })}
      />
      <label className="block text-sm text-gray-400 mb-2">Routes</label>
      {(agent.routes || []).map((route, idx) => (
        <div key={idx} className="flex gap-1 mb-2 items-center">
          <input
            className="flex-1 px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs"
            placeholder="condition"
            value={route.condition}
            onChange={(e) => {
              const routes = [...(agent.routes || [])];
              routes[idx] = { ...route, condition: e.target.value };
              onUpdate(nodeId, { routes });
            }}
          />
          <span className="text-gray-500">→</span>
          <input
            className="flex-1 px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs"
            placeholder="target"
            value={route.target}
            onChange={(e) => {
              const routes = [...(agent.routes || [])];
              routes[idx] = { ...route, target: e.target.value };
              onUpdate(nodeId, { routes });
            }}
          />
          <button className="text-red-400 hover:text-red-300 text-sm" onClick={() => onUpdate(nodeId, { routes: (agent.routes || []).filter((_, i) => i !== idx) })}>×</button>
        </div>
      ))}
      <button className="w-full py-1 bg-gray-700 hover:bg-gray-600 rounded text-xs" onClick={() => onUpdate(nodeId, { routes: [...(agent.routes || []), { condition: '', target: '' }] })}>+ Add Route</button>
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
          <input
            className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm"
            value={agent.model || ''}
            onChange={(e) => onUpdate(nodeId, { model: e.target.value })}
            placeholder="gemini-2.0-flash"
          />
        </Field>
        <Field label="Instruction">
          <textarea
            className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-24"
            value={agent.instruction}
            onChange={(e) => onUpdate(nodeId, { instruction: e.target.value })}
            placeholder="You are a helpful assistant..."
          />
        </Field>
        <Field label="Description" hint="Optional agent description">
          <input
            className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm"
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
                <span key={t} className={`text-xs px-2 py-1 rounded flex items-center gap-1 cursor-pointer ${config ? 'bg-green-800' : 'bg-gray-700'} hover:bg-gray-600`} onClick={() => onSelectTool(toolId)}>
                  {tool?.icon} {displayName} <span className="text-blue-400">⚙</span>
                  <button onClick={(e) => { e.stopPropagation(); onRemoveTool(t); }} className="ml-1 text-red-400 hover:text-red-300">×</button>
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
          className="w-full text-left text-xs text-gray-400 hover:text-gray-300 flex items-center gap-1"
        >
          <span>{showAdvanced ? '▼' : '▶'}</span> Advanced Settings
        </button>
        
        {showAdvanced && (
          <div className="mt-2 space-y-3 pl-2 border-l border-gray-700">
            <Field label="Global Instruction" hint="System-level instruction prepended to all prompts">
              <textarea
                className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs h-16"
                value={agent.global_instruction || ''}
                onChange={(e) => onUpdate(nodeId, { global_instruction: e.target.value })}
                placeholder="Always respond in JSON format..."
              />
            </Field>
            <Field label="Output Key" hint="Custom state key for agent output">
              <input
                className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs"
                value={agent.output_key || ''}
                onChange={(e) => onUpdate(nodeId, { output_key: e.target.value })}
                placeholder="response (default)"
              />
            </Field>
            <Field label="Output Schema" hint="JSON Schema for structured output">
              <textarea
                className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs h-20 font-mono"
                value={agent.output_schema || ''}
                onChange={(e) => onUpdate(nodeId, { output_schema: e.target.value })}
                placeholder='{"type": "object", "properties": {...}}'
              />
            </Field>
            <Field label="Include Contents">
              <select
                className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs"
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
      <div className="text-xs font-semibold text-gray-400 uppercase tracking-wide mb-2">{title}</div>
      <div className="space-y-2">{children}</div>
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="block text-sm text-gray-400 mb-1">
        {label}
        {hint && <span className="text-xs text-gray-500 ml-1">({hint})</span>}
      </label>
      {children}
    </div>
  );
}
