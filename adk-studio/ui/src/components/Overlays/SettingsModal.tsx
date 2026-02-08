import React, { useState, useEffect } from 'react';
import type { ProjectSettings } from '../../types/project';
import { PROVIDERS } from '../../data/models';

interface Props {
  settings: ProjectSettings;
  projectName: string;
  projectDescription: string;
  onSave: (settings: ProjectSettings, name: string, description: string) => void;
  onClose: () => void;
}

type SettingsTab = 'general' | 'codegen' | 'ui' | 'env';

const ADK_VERSIONS = ['0.3.0', '0.2.1', '0.2.0', '0.1.9', '0.1.0'];
const RUST_EDITIONS = ['2024', '2021'] as const;

export function SettingsModal({ settings, projectName, projectDescription, onSave, onClose }: Props) {
  const [activeTab, setActiveTab] = useState<SettingsTab>('general');
  const [localSettings, setLocalSettings] = useState<ProjectSettings>({ ...settings });
  const [name, setName] = useState(projectName);
  const [description, setDescription] = useState(projectDescription);
  const [envKey, setEnvKey] = useState('');
  const [envValue, setEnvValue] = useState('');

  // Initialize defaults
  useEffect(() => {
    setLocalSettings({
      ...settings,
      adkVersion: settings.adkVersion || '0.3.0',
      rustEdition: settings.rustEdition || '2024',
      defaultProvider: settings.defaultProvider || 'gemini',
      default_model: settings.default_model || 'gemini-2.5-flash',
      autobuildEnabled: settings.autobuildEnabled ?? true,
      showMinimap: settings.showMinimap ?? true,
      showTimeline: settings.showTimeline ?? true,
      consolePosition: settings.consolePosition || 'bottom',
    });
  }, [settings]);

  const handleSave = () => {
    onSave(localSettings, name, description);
    onClose();
  };

  const updateSetting = <K extends keyof ProjectSettings>(key: K, value: ProjectSettings[K]) => {
    setLocalSettings(prev => ({ ...prev, [key]: value }));
  };

  const addEnvVar = () => {
    if (!envKey.trim()) return;
    setLocalSettings(prev => ({
      ...prev,
      env_vars: { ...prev.env_vars, [envKey.trim()]: envValue },
    }));
    setEnvKey('');
    setEnvValue('');
  };

  const removeEnvVar = (key: string) => {
    setLocalSettings(prev => {
      const { [key]: _, ...rest } = prev.env_vars;
      return { ...prev, env_vars: rest };
    });
  };

  const selectedProvider = PROVIDERS.find(p => p.id === localSettings.defaultProvider) || PROVIDERS[0];

  const tabs: { id: SettingsTab; label: string; icon: string }[] = [
    { id: 'general', label: 'General', icon: '📋' },
    { id: 'codegen', label: 'Code Generation', icon: '⚙️' },
    { id: 'ui', label: 'UI Preferences', icon: '🎨' },
    { id: 'env', label: 'Environment', icon: '🔐' },
  ];

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={onClose}>
      <div
        className="rounded-lg w-[600px] max-h-[80vh] flex flex-col"
        style={{ backgroundColor: 'var(--surface-panel)' }}
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div
          className="flex justify-between items-center p-4 border-b"
          style={{ borderColor: 'var(--border-default)' }}
        >
          <h2 className="text-lg font-semibold" style={{ color: 'var(--text-primary)' }}>
            ⚙️ Project Settings
          </h2>
          <button
            onClick={onClose}
            className="text-xl hover:opacity-70"
            style={{ color: 'var(--text-muted)' }}
          >
            ×
          </button>
        </div>

        {/* Tabs */}
        <div
          className="flex border-b"
          style={{ borderColor: 'var(--border-default)' }}
        >
          {tabs.map(tab => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className="px-4 py-2 text-sm font-medium transition-colors"
              style={{
                color: activeTab === tab.id ? 'var(--accent-primary)' : 'var(--text-secondary)',
                borderBottom: activeTab === tab.id ? '2px solid var(--accent-primary)' : '2px solid transparent',
              }}
            >
              {tab.icon} {tab.label}
            </button>
          ))}
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4">
          {activeTab === 'general' && (
            <div className="space-y-4">
              <Field label="Project Name">
                <input
                  type="text"
                  value={name}
                  onChange={e => setName(e.target.value)}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                />
              </Field>

              <Field label="Description">
                <textarea
                  value={description}
                  onChange={e => setDescription(e.target.value)}
                  rows={3}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                />
              </Field>

              <Field label="Default Provider">
                <select
                  value={localSettings.defaultProvider || 'gemini'}
                  onChange={e => {
                    const provider = PROVIDERS.find(p => p.id === e.target.value);
                    updateSetting('defaultProvider', e.target.value);
                    if (provider && provider.models.length > 0) {
                      updateSetting('default_model', provider.models[0].id);
                    }
                  }}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                >
                  {PROVIDERS.map(p => (
                    <option key={p.id} value={p.id}>{p.icon} {p.name}</option>
                  ))}
                </select>
              </Field>

              <Field label="Default Model">
                <select
                  value={localSettings.default_model}
                  onChange={e => updateSetting('default_model', e.target.value)}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                >
                  {selectedProvider.models.map(m => (
                    <option key={m.id} value={m.id}>{m.name}</option>
                  ))}
                </select>
              </Field>
            </div>
          )}

          {activeTab === 'codegen' && (
            <div className="space-y-4">
              <Field label="ADK-Rust Version" hint="Version of ADK crates to use in generated code">
                <select
                  value={localSettings.adkVersion || '0.3.0'}
                  onChange={e => updateSetting('adkVersion', e.target.value)}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                >
                  {ADK_VERSIONS.map(v => (
                    <option key={v} value={v}>{v}{v === '0.3.0' ? ' (latest)' : ''}</option>
                  ))}
                </select>
              </Field>

              <Field label="Rust Edition" hint="Rust edition for generated Cargo.toml">
                <select
                  value={localSettings.rustEdition || '2024'}
                  onChange={e => updateSetting('rustEdition', e.target.value as '2021' | '2024')}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                >
                  {RUST_EDITIONS.map(e => (
                    <option key={e} value={e}>{e}{e === '2024' ? ' (latest)' : ''}</option>
                  ))}
                </select>
              </Field>

              <div
                className="p-3 rounded text-xs"
                style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-muted)' }}
              >
                <div className="font-medium mb-1">Generated Code Preview:</div>
                <code className="block font-mono">
                  [package]<br />
                  edition = "{localSettings.rustEdition || '2024'}"<br />
                  <br />
                  [dependencies]<br />
                  adk-core = "{localSettings.adkVersion || '0.3.0'}"<br />
                  adk-agent = "{localSettings.adkVersion || '0.3.0'}"<br />
                  ...
                </code>
              </div>
            </div>
          )}

          {activeTab === 'ui' && (
            <div className="space-y-4">
              <Toggle
                label="Autobuild"
                hint="Automatically rebuild when project changes"
                checked={localSettings.autobuildEnabled ?? true}
                onChange={v => updateSetting('autobuildEnabled', v)}
              />

              {/* Autobuild Triggers - only show when autobuild is enabled */}
              {(localSettings.autobuildEnabled ?? true) && (
                <div 
                  className="p-3 rounded space-y-2"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)' }}
                >
                  <div className="text-xs font-medium mb-2" style={{ color: 'var(--text-muted)' }}>
                    Autobuild Triggers:
                  </div>
                  <div className="grid grid-cols-2 gap-2">
                    <TriggerCheckbox
                      label="Agent Add"
                      checked={localSettings.autobuildTriggers?.onAgentAdd ?? true}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...localSettings.autobuildTriggers, 
                        onAgentAdd: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Agent Delete"
                      checked={localSettings.autobuildTriggers?.onAgentDelete ?? true}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...localSettings.autobuildTriggers, 
                        onAgentDelete: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Agent Update"
                      checked={localSettings.autobuildTriggers?.onAgentUpdate ?? true}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...localSettings.autobuildTriggers, 
                        onAgentUpdate: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Tool Add"
                      checked={localSettings.autobuildTriggers?.onToolAdd ?? true}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...localSettings.autobuildTriggers, 
                        onToolAdd: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Tool Update"
                      checked={localSettings.autobuildTriggers?.onToolUpdate ?? true}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...localSettings.autobuildTriggers, 
                        onToolUpdate: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Edge Add"
                      checked={localSettings.autobuildTriggers?.onEdgeAdd ?? true}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...localSettings.autobuildTriggers, 
                        onEdgeAdd: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Edge Delete"
                      checked={localSettings.autobuildTriggers?.onEdgeDelete ?? true}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...localSettings.autobuildTriggers, 
                        onEdgeDelete: v 
                      })}
                    />
                  </div>
                </div>
              )}

              <Toggle
                label="Show Minimap"
                hint="Display minimap in canvas corner"
                checked={localSettings.showMinimap ?? true}
                onChange={v => updateSetting('showMinimap', v)}
              />

              <Toggle
                label="Show Timeline"
                hint="Display execution timeline during runs"
                checked={localSettings.showTimeline ?? true}
                onChange={v => updateSetting('showTimeline', v)}
              />

              <Toggle
                label="Show Data Flow Overlay"
                hint="Display state keys on edges"
                checked={localSettings.showDataFlowOverlay ?? false}
                onChange={v => updateSetting('showDataFlowOverlay', v)}
              />

              <Field label="Layout Mode">
                <select
                  value={localSettings.layoutMode || 'free'}
                  onChange={e => updateSetting('layoutMode', e.target.value as 'free' | 'fixed')}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                >
                  <option value="free">Free (drag anywhere)</option>
                  <option value="fixed">Fixed (auto-layout)</option>
                </select>
              </Field>

              <Field label="Layout Direction">
                <select
                  value={localSettings.layoutDirection || 'TB'}
                  onChange={e => updateSetting('layoutDirection', e.target.value as 'TB' | 'LR' | 'BT' | 'RL')}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                >
                  <option value="TB">Top to Bottom ↓</option>
                  <option value="LR">Left to Right →</option>
                  <option value="BT">Bottom to Top ↑</option>
                  <option value="RL">Right to Left ←</option>
                </select>
              </Field>
            </div>
          )}

          {activeTab === 'env' && (
            <div className="space-y-4">
              <div
                className="p-3 rounded text-xs"
                style={{ backgroundColor: 'rgba(59, 130, 246, 0.1)', border: '1px solid var(--accent-primary)', color: 'var(--text-secondary)' }}
              >
                <div className="font-medium mb-1" style={{ color: 'var(--accent-primary)' }}>💡 Environment Variables</div>
                <p>These are stored in the project and used when running the generated code. Sensitive values are stored locally only.</p>
              </div>

              {/* Add new env var */}
              <div className="flex gap-2">
                <input
                  type="text"
                  placeholder="KEY"
                  value={envKey}
                  onChange={e => setEnvKey(e.target.value.toUpperCase())}
                  className="flex-1 px-3 py-2 rounded text-sm font-mono"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                />
                <input
                  type="password"
                  placeholder="value"
                  value={envValue}
                  onChange={e => setEnvValue(e.target.value)}
                  className="flex-1 px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                />
                <button
                  onClick={addEnvVar}
                  className="px-3 py-2 rounded text-sm font-medium"
                  style={{ backgroundColor: 'var(--accent-primary)', color: 'white' }}
                >
                  Add
                </button>
              </div>

              {/* Existing env vars */}
              <div className="space-y-2">
                {Object.entries(localSettings.env_vars || {}).map(([key, value]) => (
                  <div
                    key={key}
                    className="flex items-center gap-2 p-2 rounded"
                    style={{ backgroundColor: 'var(--bg-secondary)' }}
                  >
                    <code className="flex-1 text-sm font-mono" style={{ color: 'var(--accent-primary)' }}>
                      {key}
                    </code>
                    <span className="text-sm" style={{ color: 'var(--text-muted)' }}>
                      {value ? '••••••••' : '(empty)'}
                    </span>
                    <button
                      onClick={() => removeEnvVar(key)}
                      className="text-red-500 hover:text-red-400 text-sm"
                    >
                      ✕
                    </button>
                  </div>
                ))}
                {Object.keys(localSettings.env_vars || {}).length === 0 && (
                  <div className="text-sm text-center py-4" style={{ color: 'var(--text-muted)' }}>
                    No environment variables configured
                  </div>
                )}
              </div>

              {/* Common env vars suggestions */}
              <div>
                <div className="text-xs font-medium mb-2" style={{ color: 'var(--text-muted)' }}>
                  Quick Add:
                </div>
                <div className="flex flex-wrap gap-1">
                  {['GOOGLE_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY', 'DEEPSEEK_API_KEY', 'GROQ_API_KEY'].map(key => (
                    <button
                      key={key}
                      onClick={() => {
                        if (!localSettings.env_vars?.[key]) {
                          setEnvKey(key);
                        }
                      }}
                      disabled={!!localSettings.env_vars?.[key]}
                      className="px-2 py-1 rounded text-xs"
                      style={{
                        backgroundColor: localSettings.env_vars?.[key] ? 'var(--accent-success)' : 'var(--bg-secondary)',
                        color: localSettings.env_vars?.[key] ? 'white' : 'var(--text-secondary)',
                        border: '1px solid var(--border-default)',
                        opacity: localSettings.env_vars?.[key] ? 0.7 : 1,
                      }}
                    >
                      {localSettings.env_vars?.[key] ? '✓ ' : '+ '}{key}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          )}
        </div>

        {/* Footer */}
        <div
          className="flex justify-end gap-2 p-4 border-t"
          style={{ borderColor: 'var(--border-default)' }}
        >
          <button
            onClick={onClose}
            className="px-4 py-2 rounded text-sm font-medium"
            style={{
              backgroundColor: 'var(--bg-secondary)',
              color: 'var(--text-primary)',
              border: '1px solid var(--border-default)',
            }}
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            className="px-4 py-2 rounded text-sm font-medium"
            style={{ backgroundColor: 'var(--accent-primary)', color: 'white' }}
          >
            Save Settings
          </button>
        </div>
      </div>
    </div>
  );
}

// Helper components
function Field({ label, hint, children }: { label: string; hint?: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="block text-sm font-medium mb-1" style={{ color: 'var(--text-secondary)' }}>
        {label}
        {hint && <span className="font-normal ml-1" style={{ color: 'var(--text-muted)' }}>({hint})</span>}
      </label>
      {children}
    </div>
  );
}

function Toggle({ label, hint, checked, onChange }: { label: string; hint?: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <div className="flex items-center justify-between">
      <div>
        <div className="text-sm font-medium" style={{ color: 'var(--text-secondary)' }}>{label}</div>
        {hint && <div className="text-xs" style={{ color: 'var(--text-muted)' }}>{hint}</div>}
      </div>
      <button
        onClick={() => onChange(!checked)}
        className="w-12 h-6 rounded-full transition-colors relative"
        style={{ backgroundColor: checked ? 'var(--accent-primary)' : 'var(--bg-secondary)' }}
      >
        <div
          className="w-5 h-5 rounded-full bg-white absolute top-0.5 transition-transform"
          style={{ transform: checked ? 'translateX(26px)' : 'translateX(2px)' }}
        />
      </button>
    </div>
  );
}

function TriggerCheckbox({ label, checked, onChange }: { label: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="flex items-center gap-2 cursor-pointer">
      <input
        type="checkbox"
        checked={checked}
        onChange={e => onChange(e.target.checked)}
        className="w-4 h-4 rounded"
        style={{ accentColor: 'var(--accent-primary)' }}
      />
      <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>{label}</span>
    </label>
  );
}

export default SettingsModal;
