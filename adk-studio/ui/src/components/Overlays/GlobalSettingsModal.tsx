import React, { useState, useEffect } from 'react';
import { GlobalSettings, DEFAULT_GLOBAL_SETTINGS, loadGlobalSettings, saveGlobalSettings } from '../../types/settings';
import { PROVIDERS } from '../../data/models';
import { useTheme } from '../../hooks/useTheme';

interface Props {
  onClose: () => void;
  onThemeChange?: (theme: 'light' | 'dark' | 'system') => void;
}

type SettingsTab = 'defaults' | 'codegen' | 'ui' | 'build';

const ADK_VERSIONS = ['0.3.0', '0.2.1', '0.2.0', '0.1.9', '0.1.0'];
const RUST_EDITIONS = ['2024', '2021'] as const;

export function GlobalSettingsModal({ onClose, onThemeChange }: Props) {
  const [activeTab, setActiveTab] = useState<SettingsTab>('defaults');
  const [settings, setSettings] = useState<GlobalSettings>(DEFAULT_GLOBAL_SETTINGS);
  const { mode: _mode, setMode } = useTheme();

  useEffect(() => {
    setSettings(loadGlobalSettings());
  }, []);

  const handleSave = () => {
    saveGlobalSettings(settings);
    // Apply theme change immediately
    if (onThemeChange) {
      onThemeChange(settings.theme);
    }
    if (settings.theme !== 'system') {
      setMode(settings.theme);
    }
    onClose();
  };

  const updateSetting = <K extends keyof GlobalSettings>(key: K, value: GlobalSettings[K]) => {
    setSettings(prev => ({ ...prev, [key]: value }));
  };

  const selectedProvider = PROVIDERS.find(p => p.id === settings.defaultProvider) || PROVIDERS[0];

  const tabs: { id: SettingsTab; label: string; icon: string }[] = [
    { id: 'defaults', label: 'Defaults', icon: '🎯' },
    { id: 'codegen', label: 'Code Generation', icon: '⚙️' },
    { id: 'ui', label: 'UI Preferences', icon: '🎨' },
    { id: 'build', label: 'Build Settings', icon: '🔨' },
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
            🌐 Global Settings
          </h2>
          <button
            onClick={onClose}
            className="text-xl hover:opacity-70"
            style={{ color: 'var(--text-muted)' }}
          >
            ×
          </button>
        </div>

        {/* Info banner */}
        <div
          className="mx-4 mt-4 p-3 rounded text-xs"
          style={{ backgroundColor: 'rgba(59, 130, 246, 0.1)', border: '1px solid var(--accent-primary)', color: 'var(--text-secondary)' }}
        >
          <div className="font-medium mb-1" style={{ color: 'var(--accent-primary)' }}>💡 Global Settings</div>
          <p>These settings apply as defaults for all new projects. Individual projects can override these in their own settings.</p>
        </div>

        {/* Tabs */}
        <div
          className="flex border-b mt-4"
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
          {activeTab === 'defaults' && (
            <div className="space-y-4">
              <Field label="Default Provider" hint="Provider for new agents">
                <select
                  value={settings.defaultProvider}
                  onChange={e => {
                    const provider = PROVIDERS.find(p => p.id === e.target.value);
                    updateSetting('defaultProvider', e.target.value);
                    if (provider && provider.models.length > 0) {
                      updateSetting('defaultModel', provider.models[0].id);
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

              <Field label="Default Model" hint="Model for new agents">
                <select
                  value={settings.defaultModel}
                  onChange={e => updateSetting('defaultModel', e.target.value)}
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

              <Field label="Default Layout Mode">
                <select
                  value={settings.layoutMode}
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

              <Field label="Default Layout Direction">
                <select
                  value={settings.layoutDirection}
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

          {activeTab === 'codegen' && (
            <div className="space-y-4">
              <Field label="ADK-Rust Version" hint="Default version for generated code">
                <select
                  value={settings.adkVersion}
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

              <Field label="Rust Edition" hint="Default edition for Cargo.toml">
                <select
                  value={settings.rustEdition}
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
                  edition = "{settings.rustEdition}"<br />
                  <br />
                  [dependencies]<br />
                  adk-core = "{settings.adkVersion}"<br />
                  adk-agent = "{settings.adkVersion}"<br />
                  ...
                </code>
              </div>
            </div>
          )}

          {activeTab === 'ui' && (
            <div className="space-y-4">
              <Field label="Theme">
                <select
                  value={settings.theme}
                  onChange={e => updateSetting('theme', e.target.value as 'light' | 'dark' | 'system')}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                >
                  <option value="dark">🌙 Dark</option>
                  <option value="light">☀️ Light</option>
                  <option value="system">💻 System</option>
                </select>
              </Field>

              <Toggle
                label="Show Minimap"
                hint="Display minimap in canvas corner by default"
                checked={settings.showMinimap}
                onChange={v => updateSetting('showMinimap', v)}
              />

              <Toggle
                label="Show Timeline"
                hint="Display execution timeline during runs by default"
                checked={settings.showTimeline}
                onChange={v => updateSetting('showTimeline', v)}
              />

              <Toggle
                label="Show Data Flow Overlay"
                hint="Display state keys on edges by default"
                checked={settings.showDataFlowOverlay}
                onChange={v => updateSetting('showDataFlowOverlay', v)}
              />
            </div>
          )}

          {activeTab === 'build' && (
            <div className="space-y-4">
              <Toggle
                label="Autobuild Enabled"
                hint="Automatically rebuild when project changes"
                checked={settings.autobuildEnabled}
                onChange={v => updateSetting('autobuildEnabled', v)}
              />

              {settings.autobuildEnabled && (
                <div 
                  className="p-3 rounded space-y-2"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)' }}
                >
                  <div className="text-xs font-medium mb-2" style={{ color: 'var(--text-muted)' }}>
                    Default Autobuild Triggers:
                  </div>
                  <div className="grid grid-cols-2 gap-2">
                    <TriggerCheckbox
                      label="Agent Add"
                      checked={settings.autobuildTriggers.onAgentAdd}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...settings.autobuildTriggers, 
                        onAgentAdd: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Agent Delete"
                      checked={settings.autobuildTriggers.onAgentDelete}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...settings.autobuildTriggers, 
                        onAgentDelete: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Agent Update"
                      checked={settings.autobuildTriggers.onAgentUpdate}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...settings.autobuildTriggers, 
                        onAgentUpdate: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Tool Add"
                      checked={settings.autobuildTriggers.onToolAdd}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...settings.autobuildTriggers, 
                        onToolAdd: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Tool Update"
                      checked={settings.autobuildTriggers.onToolUpdate}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...settings.autobuildTriggers, 
                        onToolUpdate: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Edge Add"
                      checked={settings.autobuildTriggers.onEdgeAdd}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...settings.autobuildTriggers, 
                        onEdgeAdd: v 
                      })}
                    />
                    <TriggerCheckbox
                      label="Edge Delete"
                      checked={settings.autobuildTriggers.onEdgeDelete}
                      onChange={v => updateSetting('autobuildTriggers', { 
                        ...settings.autobuildTriggers, 
                        onEdgeDelete: v 
                      })}
                    />
                  </div>
                </div>
              )}
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

export default GlobalSettingsModal;
