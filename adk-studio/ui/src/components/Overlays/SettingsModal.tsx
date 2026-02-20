import React, { useState, useEffect } from 'react';
import type { ProjectSettings } from '../../types/project';
import { GlobalSettings, DEFAULT_GLOBAL_SETTINGS, loadGlobalSettings, saveGlobalSettings } from '../../types/settings';
import { PROVIDERS } from '../../data/models';
import { ApiKeyRow } from './ApiKeyRow';
import { useApiKeys } from '../../hooks/useApiKeys';
import { useTheme } from '../../hooks/useTheme';
import { api, DetectedKeyEntry } from '../../api/client';

/** Known sensitive key patterns — matches backend KNOWN_PROVIDER_KEYS + common patterns. */
const SENSITIVE_KEY_PATTERNS = new Set([
  'GOOGLE_API_KEY', 'GEMINI_API_KEY', 'OPENAI_API_KEY', 'ANTHROPIC_API_KEY',
  'DEEPSEEK_API_KEY', 'GROQ_API_KEY', 'OLLAMA_HOST', 'GITHUB_TOKEN', 'SLACK_BOT_TOKEN',
]);

function isSensitiveKey(name: string): boolean {
  if (SENSITIVE_KEY_PATTERNS.has(name)) return true;
  const upper = name.toUpperCase();
  return upper.endsWith('_API_KEY') || upper.endsWith('_TOKEN') || upper.endsWith('_SECRET');
}

/** Provider display metadata keyed by environment variable name. */
const PROVIDER_KEY_META: Record<string, { name: string; icon: string }> = {
  GOOGLE_API_KEY: { name: 'Google Gemini', icon: '✨' },
  GEMINI_API_KEY: { name: 'Google Gemini', icon: '✨' },
  OPENAI_API_KEY: { name: 'OpenAI', icon: '🤖' },
  ANTHROPIC_API_KEY: { name: 'Anthropic Claude', icon: '🎭' },
  DEEPSEEK_API_KEY: { name: 'DeepSeek', icon: '🔍' },
  GROQ_API_KEY: { name: 'Groq', icon: '⚡' },
  OLLAMA_HOST: { name: 'Ollama (Local)', icon: '🦙' },
  GITHUB_TOKEN: { name: 'GitHub', icon: '🐙' },
  SLACK_BOT_TOKEN: { name: 'Slack', icon: '💬' },
};

type SettingsScope = 'project' | 'global';
type SettingsTab = 'general' | 'codegen' | 'ui' | 'env';

const ADK_VERSIONS = ['0.3.0', '0.2.1', '0.2.0', '0.1.9', '0.1.0'];
const RUST_EDITIONS = ['2024', '2021'] as const;

interface Props {
  /** If provided, the modal opens in project scope with project-specific settings. */
  settings?: ProjectSettings;
  projectName?: string;
  projectDescription?: string;
  projectId?: string;
  initialTab?: SettingsTab;
  showApiKeyBanner?: boolean;
  /** Called when saving project-level settings. */
  onSave?: (settings: ProjectSettings, name: string, description: string) => void;
  onClose: () => void;
  /** Called when global theme changes. */
  onThemeChange?: (theme: 'light' | 'dark' | 'system') => void;
}

export function SettingsModal({
  settings,
  projectName: initialProjectName,
  projectDescription: initialProjectDescription,
  projectId,
  initialTab,
  showApiKeyBanner,
  onSave,
  onClose,
  onThemeChange,
}: Props) {
  const hasProject = !!settings;
  const [scope, setScope] = useState<SettingsScope>(hasProject ? 'project' : 'global');
  const [activeTab, setActiveTab] = useState<SettingsTab>(initialTab || 'general');

  // Project-level state
  const [localSettings, setLocalSettings] = useState<ProjectSettings>(settings ? { ...settings } : {} as ProjectSettings);
  const [name, setName] = useState(initialProjectName || '');
  const [description, setDescription] = useState(initialProjectDescription || '');
  const [envKey, setEnvKey] = useState('');
  const [envValue, setEnvValue] = useState('');
  const { keys: apiKeys, isLoading: apiKeysLoading, saveKey, removeKey } = useApiKeys(projectId ?? null);

  // Global-level state
  const [globalSettings, setGlobalSettings] = useState<GlobalSettings>(DEFAULT_GLOBAL_SETTINGS);
  const [detectedKeys, setDetectedKeys] = useState<DetectedKeyEntry[]>([]);
  const [keysLoading, setKeysLoading] = useState(false);
  const { setMode } = useTheme();

  useEffect(() => {
    setGlobalSettings(loadGlobalSettings());
    setKeysLoading(true);
    api.settings.getDetectedKeys()
      .then(res => setDetectedKeys(res.keys))
      .catch(() => {})
      .finally(() => setKeysLoading(false));
  }, []);

  useEffect(() => {
    if (settings) {
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
    }
  }, [settings]);

  const handleSave = () => {
    if (scope === 'project' && onSave) {
      onSave(localSettings, name, description);
    }
    if (scope === 'global') {
      saveGlobalSettings(globalSettings);
      if (onThemeChange) onThemeChange(globalSettings.theme);
      if (globalSettings.theme !== 'system') setMode(globalSettings.theme);
    }
    onClose();
  };

  const updateProjectSetting = <K extends keyof ProjectSettings>(key: K, value: ProjectSettings[K]) => {
    setLocalSettings(prev => ({ ...prev, [key]: value }));
  };

  const updateGlobalSetting = <K extends keyof GlobalSettings>(key: K, value: GlobalSettings[K]) => {
    setGlobalSettings(prev => ({ ...prev, [key]: value }));
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

  const customEnvVars = Object.entries(localSettings.env_vars || {}).filter(
    ([key]) => !isSensitiveKey(key)
  );

  // Provider/model for the active scope
  const activeProvider = scope === 'project'
    ? (localSettings.defaultProvider || 'gemini')
    : globalSettings.defaultProvider;
  const selectedProvider = PROVIDERS.find(p => p.id === activeProvider) || PROVIDERS[0];

  const tabs: { id: SettingsTab; label: string; icon: string }[] = [
    { id: 'general', label: 'General', icon: '📋' },
    { id: 'codegen', label: 'Code Gen', icon: '⚙️' },
    { id: 'ui', label: 'UI', icon: '🎨' },
    { id: 'env', label: 'Environment', icon: '🔐' },
  ];

  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={onClose}>
      <div
        className="rounded-lg w-[620px] max-h-[80vh] flex flex-col"
        style={{ backgroundColor: 'var(--surface-panel)' }}
        onClick={e => e.stopPropagation()}
      >
        {/* Header */}
        <div
          className="flex justify-between items-center p-4 border-b"
          style={{ borderColor: 'var(--border-default)' }}
        >
          <h2 className="text-lg font-semibold" style={{ color: 'var(--text-primary)' }}>
            ⚙️ Settings
          </h2>
          <button
            onClick={onClose}
            className="text-xl hover:opacity-70"
            style={{ color: 'var(--text-muted)' }}
          >
            ×
          </button>
        </div>

        {/* Scope toggle — only show when a project is open */}
        {hasProject && (
          <div
            className="flex mx-4 mt-3 rounded overflow-hidden"
            style={{ border: '1px solid var(--border-default)' }}
          >
            {(['project', 'global'] as const).map(s => (
              <button
                key={s}
                onClick={() => setScope(s)}
                className="flex-1 px-3 py-1.5 text-sm font-medium transition-colors"
                style={{
                  backgroundColor: scope === s ? 'var(--accent-primary)' : 'transparent',
                  color: scope === s ? 'white' : 'var(--text-secondary)',
                }}
              >
                {s === 'project' ? '📁 Project' : '🌐 Global Defaults'}
              </button>
            ))}
          </div>
        )}

        {/* Scope hint */}
        {scope === 'global' && (
          <div
            className="mx-4 mt-3 p-2 rounded text-xs"
            style={{ backgroundColor: 'rgba(59, 130, 246, 0.1)', border: '1px solid var(--accent-primary)', color: 'var(--text-secondary)' }}
          >
            💡 Global settings apply as defaults for all new projects. Projects can override these individually.
          </div>
        )}

        {/* Tabs */}
        <div
          className="flex border-b mt-3"
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
          {activeTab === 'general' && scope === 'project' && (
            <div className="space-y-4">
              <Field label="Project Name">
                <input
                  type="text"
                  value={name}
                  onChange={e => setName(e.target.value)}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                />
              </Field>
              <Field label="Description">
                <textarea
                  value={description}
                  onChange={e => setDescription(e.target.value)}
                  rows={3}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                />
              </Field>
              <Field label="Default Provider">
                <select
                  value={localSettings.defaultProvider || 'gemini'}
                  onChange={e => {
                    const provider = PROVIDERS.find(p => p.id === e.target.value);
                    updateProjectSetting('defaultProvider', e.target.value);
                    if (provider?.models.length) updateProjectSetting('default_model', provider.models[0].id);
                  }}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                >
                  {PROVIDERS.map(p => <option key={p.id} value={p.id}>{p.icon} {p.name}</option>)}
                </select>
              </Field>
              <Field label="Default Model">
                <select
                  value={localSettings.default_model}
                  onChange={e => updateProjectSetting('default_model', e.target.value)}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                >
                  {selectedProvider.models.map(m => <option key={m.id} value={m.id}>{m.name}</option>)}
                </select>
              </Field>
            </div>
          )}

          {activeTab === 'general' && scope === 'global' && (
            <div className="space-y-4">
              <Field label="Default Provider" hint="Provider for new projects">
                <select
                  value={globalSettings.defaultProvider}
                  onChange={e => {
                    const provider = PROVIDERS.find(p => p.id === e.target.value);
                    updateGlobalSetting('defaultProvider', e.target.value);
                    if (provider?.models.length) updateGlobalSetting('defaultModel', provider.models[0].id);
                  }}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                >
                  {PROVIDERS.map(p => <option key={p.id} value={p.id}>{p.icon} {p.name}</option>)}
                </select>
              </Field>
              <Field label="Default Model" hint="Model for new projects">
                <select
                  value={globalSettings.defaultModel}
                  onChange={e => updateGlobalSetting('defaultModel', e.target.value)}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                >
                  {selectedProvider.models.map(m => <option key={m.id} value={m.id}>{m.name}</option>)}
                </select>
              </Field>
              <Field label="Default Layout Mode">
                <select
                  value={globalSettings.layoutMode}
                  onChange={e => updateGlobalSetting('layoutMode', e.target.value as 'free' | 'fixed')}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                >
                  <option value="free">Free (drag anywhere)</option>
                  <option value="fixed">Fixed (auto-layout)</option>
                </select>
              </Field>
              <Field label="Default Layout Direction">
                <select
                  value={globalSettings.layoutDirection}
                  onChange={e => updateGlobalSetting('layoutDirection', e.target.value as 'TB' | 'LR' | 'BT' | 'RL')}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
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
              <Field label="ADK-Rust Version" hint="Version of ADK crates in generated code">
                <select
                  value={scope === 'project' ? (localSettings.adkVersion || '0.3.0') : globalSettings.adkVersion}
                  onChange={e => scope === 'project' ? updateProjectSetting('adkVersion', e.target.value) : updateGlobalSetting('adkVersion', e.target.value)}
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                >
                  {ADK_VERSIONS.map(v => <option key={v} value={v}>{v}{v === '0.3.0' ? ' (latest)' : ''}</option>)}
                </select>
              </Field>
              <Field label="Rust Edition" hint="Edition for generated Cargo.toml">
                <select
                  value={scope === 'project' ? (localSettings.rustEdition || '2024') : globalSettings.rustEdition}
                  onChange={e => scope === 'project'
                    ? updateProjectSetting('rustEdition', e.target.value as '2021' | '2024')
                    : updateGlobalSetting('rustEdition', e.target.value as '2021' | '2024')
                  }
                  className="w-full px-3 py-2 rounded text-sm"
                  style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                >
                  {RUST_EDITIONS.map(e => <option key={e} value={e}>{e}{e === '2024' ? ' (latest)' : ''}</option>)}
                </select>
              </Field>
              <div
                className="p-3 rounded text-xs"
                style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-muted)' }}
              >
                <div className="font-medium mb-1">Generated Code Preview:</div>
                <code className="block font-mono">
                  [package]<br />
                  edition = "{scope === 'project' ? (localSettings.rustEdition || '2024') : globalSettings.rustEdition}"<br />
                  <br />
                  [dependencies]<br />
                  adk-core = "{scope === 'project' ? (localSettings.adkVersion || '0.3.0') : globalSettings.adkVersion}"<br />
                  adk-agent = "{scope === 'project' ? (localSettings.adkVersion || '0.3.0') : globalSettings.adkVersion}"<br />
                  ...
                </code>
              </div>
            </div>
          )}

          {activeTab === 'ui' && (
            <div className="space-y-4">
              {/* Theme — global only */}
              {scope === 'global' && (
                <Field label="Theme">
                  <select
                    value={globalSettings.theme}
                    onChange={e => updateGlobalSetting('theme', e.target.value as 'light' | 'dark' | 'system')}
                    className="w-full px-3 py-2 rounded text-sm"
                    style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                  >
                    <option value="dark">🌙 Dark</option>
                    <option value="light">☀️ Light</option>
                    <option value="system">💻 System</option>
                  </select>
                </Field>
              )}

              <Toggle
                label="Show Minimap"
                hint="Display minimap in canvas corner"
                checked={scope === 'project' ? (localSettings.showMinimap ?? true) : globalSettings.showMinimap}
                onChange={v => scope === 'project' ? updateProjectSetting('showMinimap', v) : updateGlobalSetting('showMinimap', v)}
              />
              <Toggle
                label="Show Timeline"
                hint="Display execution timeline during runs"
                checked={scope === 'project' ? (localSettings.showTimeline ?? true) : globalSettings.showTimeline}
                onChange={v => scope === 'project' ? updateProjectSetting('showTimeline', v) : updateGlobalSetting('showTimeline', v)}
              />
              <Toggle
                label="Show Data Flow Overlay"
                hint="Display state keys on edges"
                checked={scope === 'project' ? (localSettings.showDataFlowOverlay ?? false) : globalSettings.showDataFlowOverlay}
                onChange={v => scope === 'project' ? updateProjectSetting('showDataFlowOverlay', v) : updateGlobalSetting('showDataFlowOverlay', v)}
              />

              {scope === 'project' && (
                <>
                  <Field label="Layout Mode">
                    <select
                      value={localSettings.layoutMode || 'free'}
                      onChange={e => updateProjectSetting('layoutMode', e.target.value as 'free' | 'fixed')}
                      className="w-full px-3 py-2 rounded text-sm"
                      style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                    >
                      <option value="free">Free (drag anywhere)</option>
                      <option value="fixed">Fixed (auto-layout)</option>
                    </select>
                  </Field>
                  <Field label="Layout Direction">
                    <select
                      value={localSettings.layoutDirection || 'TB'}
                      onChange={e => updateProjectSetting('layoutDirection', e.target.value as 'TB' | 'LR' | 'BT' | 'RL')}
                      className="w-full px-3 py-2 rounded text-sm"
                      style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                    >
                      <option value="TB">Top to Bottom ↓</option>
                      <option value="LR">Left to Right →</option>
                      <option value="BT">Bottom to Top ↑</option>
                      <option value="RL">Right to Left ←</option>
                    </select>
                  </Field>
                </>
              )}
            </div>
          )}

          {activeTab === 'env' && scope === 'project' && (
            <div className="space-y-4">
              {showApiKeyBanner && (
                <div
                  className="p-3 rounded text-sm flex items-start gap-2"
                  style={{
                    backgroundColor: 'rgba(245, 158, 11, 0.1)',
                    border: '1px solid rgba(245, 158, 11, 0.4)',
                    color: 'var(--text-primary)',
                  }}
                >
                  <span className="text-base leading-none mt-0.5">⚠️</span>
                  <span>Set up your API key to get started. Your workflow requires a configured provider key to run.</span>
                </div>
              )}

              <CollapsibleSection title="🔑 API Keys" defaultOpen>
                {apiKeysLoading ? (
                  <div className="text-sm text-center py-4" style={{ color: 'var(--text-muted)' }}>Loading API keys...</div>
                ) : apiKeys.length === 0 ? (
                  <div className="text-sm text-center py-4" style={{ color: 'var(--text-muted)' }}>No API key providers available</div>
                ) : (
                  <div className="space-y-2">
                    {apiKeys.map(provider => (
                      <ApiKeyRow key={provider.name} provider={provider} onSave={saveKey} onRemove={removeKey} />
                    ))}
                  </div>
                )}
              </CollapsibleSection>

              <CollapsibleSection title="📝 Custom Environment Variables" defaultOpen>
                <div
                  className="p-3 rounded text-xs mb-3"
                  style={{ backgroundColor: 'rgba(59, 130, 246, 0.1)', border: '1px solid var(--accent-primary)', color: 'var(--text-secondary)' }}
                >
                  <p>Non-sensitive environment variables passed to the generated code at runtime.</p>
                </div>
                <div className="flex gap-2 mb-3">
                  <input
                    type="text"
                    placeholder="KEY"
                    value={envKey}
                    onChange={e => setEnvKey(e.target.value.toUpperCase())}
                    className="flex-1 px-3 py-2 rounded text-sm font-mono"
                    style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                  />
                  <input
                    type="text"
                    placeholder="value"
                    value={envValue}
                    onChange={e => setEnvValue(e.target.value)}
                    className="flex-1 px-3 py-2 rounded text-sm"
                    style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)', color: 'var(--text-primary)' }}
                  />
                  <button
                    onClick={addEnvVar}
                    className="px-3 py-2 rounded text-sm font-medium"
                    style={{ backgroundColor: 'var(--accent-primary)', color: 'white' }}
                  >
                    Add
                  </button>
                </div>
                <div className="space-y-2">
                  {customEnvVars.map(([key, value]) => (
                    <div key={key} className="flex items-center gap-2 p-2 rounded" style={{ backgroundColor: 'var(--bg-secondary)' }}>
                      <code className="flex-1 text-sm font-mono" style={{ color: 'var(--accent-primary)' }}>{key}</code>
                      <span className="text-sm" style={{ color: 'var(--text-muted)' }}>{value ? '••••••••' : '(empty)'}</span>
                      <button onClick={() => removeEnvVar(key)} className="text-red-500 hover:text-red-400 text-sm">✕</button>
                    </div>
                  ))}
                  {customEnvVars.length === 0 && (
                    <div className="text-sm text-center py-4" style={{ color: 'var(--text-muted)' }}>No custom environment variables configured</div>
                  )}
                </div>
              </CollapsibleSection>

              <CollapsibleSection title="🔨 Build Settings" defaultOpen>
                <div className="space-y-4">
                  <Toggle
                    label="Autobuild"
                    hint="Automatically rebuild when project changes"
                    checked={localSettings.autobuildEnabled ?? true}
                    onChange={v => updateProjectSetting('autobuildEnabled', v)}
                  />
                  {(localSettings.autobuildEnabled ?? true) && (
                    <AutobuildTriggersGrid
                      triggers={{
                        onAgentAdd: localSettings.autobuildTriggers?.onAgentAdd ?? true,
                        onAgentDelete: localSettings.autobuildTriggers?.onAgentDelete ?? true,
                        onAgentUpdate: localSettings.autobuildTriggers?.onAgentUpdate ?? true,
                        onToolAdd: localSettings.autobuildTriggers?.onToolAdd ?? true,
                        onToolUpdate: localSettings.autobuildTriggers?.onToolUpdate ?? true,
                        onEdgeAdd: localSettings.autobuildTriggers?.onEdgeAdd ?? true,
                        onEdgeDelete: localSettings.autobuildTriggers?.onEdgeDelete ?? true,
                      }}
                      onChange={triggers => updateProjectSetting('autobuildTriggers', triggers)}
                    />
                  )}
                </div>
              </CollapsibleSection>
            </div>
          )}

          {activeTab === 'env' && scope === 'global' && (
            <div className="space-y-4">
              <div
                className="p-3 rounded text-xs"
                style={{ backgroundColor: 'rgba(59, 130, 246, 0.1)', border: '1px solid var(--accent-primary)', color: 'var(--text-secondary)' }}
              >
                <div className="font-medium mb-1" style={{ color: 'var(--accent-primary)' }}>ℹ️ Environment API Keys</div>
                <p>
                  Global API keys are detected from your <code className="font-mono">.env</code> file
                  or system environment variables. To add or change a key, update
                  your <code className="font-mono">.env</code> file and restart Studio.
                  Per-project overrides can be set in the Project scope.
                </p>
              </div>

              {keysLoading ? (
                <div className="text-sm text-center py-4" style={{ color: 'var(--text-muted)' }}>Loading detected keys…</div>
              ) : detectedKeys.length === 0 ? (
                <div className="text-sm text-center py-4" style={{ color: 'var(--text-muted)' }}>
                  No provider keys detected. Add keys to your <code className="font-mono">.env</code> file.
                </div>
              ) : (
                <div className="space-y-2">
                  {detectedKeys.map(entry => {
                    const meta = PROVIDER_KEY_META[entry.name];
                    const isDetected = entry.status === 'detected';
                    return (
                      <div key={entry.name} className="p-3 rounded flex items-center justify-between" style={{ backgroundColor: 'var(--bg-secondary)' }}>
                        <div className="flex items-center gap-2">
                          <span className="text-base" role="img" aria-label={meta?.name ?? entry.name}>{meta?.icon ?? '🔑'}</span>
                          <div>
                            <div className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>{meta?.name ?? entry.name}</div>
                            <code className="text-xs font-mono" style={{ color: 'var(--text-muted)' }}>{entry.name}</code>
                          </div>
                        </div>
                        {isDetected ? (
                          <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs"
                            style={{ backgroundColor: 'rgba(16, 185, 129, 0.15)', color: 'var(--accent-success, #10b981)' }}>
                            ✅ detected{entry.masked && <span className="font-mono ml-1">{entry.masked}</span>}
                          </span>
                        ) : (
                          <span className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs"
                            style={{ backgroundColor: 'rgba(107, 114, 128, 0.15)', color: 'var(--text-muted)' }}>
                            ⚪ not set
                          </span>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}

              {/* Global build defaults */}
              <CollapsibleSection title="🔨 Build Defaults" defaultOpen>
                <div className="space-y-4">
                  <Toggle
                    label="Autobuild Enabled"
                    hint="Default autobuild for new projects"
                    checked={globalSettings.autobuildEnabled}
                    onChange={v => updateGlobalSetting('autobuildEnabled', v)}
                  />
                  {globalSettings.autobuildEnabled && (
                    <AutobuildTriggersGrid
                      triggers={globalSettings.autobuildTriggers}
                      onChange={triggers => updateGlobalSetting('autobuildTriggers', triggers)}
                    />
                  )}
                </div>
              </CollapsibleSection>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex justify-end gap-2 p-4 border-t" style={{ borderColor: 'var(--border-default)' }}>
          <button
            onClick={onClose}
            className="px-4 py-2 rounded text-sm font-medium"
            style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
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

// --- Helper components ---

function CollapsibleSection({ title, defaultOpen = false, children }: { title: string; defaultOpen?: boolean; children: React.ReactNode }) {
  const [isOpen, setIsOpen] = useState(defaultOpen);
  return (
    <div className="rounded" style={{ border: '1px solid var(--border-default)' }}>
      <button
        onClick={() => setIsOpen(!isOpen)}
        className="w-full flex items-center justify-between px-3 py-2 text-sm font-medium"
        style={{ color: 'var(--text-primary)' }}
      >
        <span>{title}</span>
        <span className="text-xs transition-transform" style={{ color: 'var(--text-muted)', transform: isOpen ? 'rotate(180deg)' : 'rotate(0deg)' }}>▼</span>
      </button>
      {isOpen && <div className="px-3 pb-3">{children}</div>}
    </div>
  );
}

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
      <input type="checkbox" checked={checked} onChange={e => onChange(e.target.checked)} className="w-4 h-4 rounded" style={{ accentColor: 'var(--accent-primary)' }} />
      <span className="text-xs" style={{ color: 'var(--text-secondary)' }}>{label}</span>
    </label>
  );
}

interface AutobuildTriggersGridProps {
  triggers: { onAgentAdd: boolean; onAgentDelete: boolean; onAgentUpdate: boolean; onToolAdd: boolean; onToolUpdate: boolean; onEdgeAdd: boolean; onEdgeDelete: boolean };
  onChange: (triggers: AutobuildTriggersGridProps['triggers']) => void;
}

function AutobuildTriggersGrid({ triggers, onChange }: AutobuildTriggersGridProps) {
  const update = (key: keyof typeof triggers, v: boolean) => onChange({ ...triggers, [key]: v });
  return (
    <div className="p-3 rounded space-y-2" style={{ backgroundColor: 'var(--bg-secondary)', border: '1px solid var(--border-default)' }}>
      <div className="text-xs font-medium mb-2" style={{ color: 'var(--text-muted)' }}>Autobuild Triggers:</div>
      <div className="grid grid-cols-2 gap-2">
        <TriggerCheckbox label="Agent Add" checked={triggers.onAgentAdd} onChange={v => update('onAgentAdd', v)} />
        <TriggerCheckbox label="Agent Delete" checked={triggers.onAgentDelete} onChange={v => update('onAgentDelete', v)} />
        <TriggerCheckbox label="Agent Update" checked={triggers.onAgentUpdate} onChange={v => update('onAgentUpdate', v)} />
        <TriggerCheckbox label="Tool Add" checked={triggers.onToolAdd} onChange={v => update('onToolAdd', v)} />
        <TriggerCheckbox label="Tool Update" checked={triggers.onToolUpdate} onChange={v => update('onToolUpdate', v)} />
        <TriggerCheckbox label="Edge Add" checked={triggers.onEdgeAdd} onChange={v => update('onEdgeAdd', v)} />
        <TriggerCheckbox label="Edge Delete" checked={triggers.onEdgeDelete} onChange={v => update('onEdgeDelete', v)} />
      </div>
    </div>
  );
}

export default SettingsModal;
