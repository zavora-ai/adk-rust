import React from 'react';
import {
  PROVIDERS,
  detectProviderFromModel,
  getCapabilityIcon,
  getTierColor,
  formatContextWindow,
  type ProviderInfo,
  type ModelInfo,
} from '../../data/models';

interface Props {
  value: string;
  onChange: (model: string) => void;
  compact?: boolean;
}

export function ModelSelector({ value, onChange, compact = false }: Props) {
  const [selectedProvider, setSelectedProvider] = React.useState<string>(() => 
    detectProviderFromModel(value)
  );
  const [isOpen, setIsOpen] = React.useState(false);
  const [searchQuery, setSearchQuery] = React.useState('');
  const dropdownRef = React.useRef<HTMLDivElement>(null);

  const provider = PROVIDERS.find(p => p.id === selectedProvider) || PROVIDERS[0];
  const currentModel = provider.models.find(m => m.id === value);

  // Close dropdown when clicking outside
  React.useEffect(() => {
    function handleClickOutside(event: MouseEvent) {
      if (dropdownRef.current && !dropdownRef.current.contains(event.target as Node)) {
        setIsOpen(false);
      }
    }
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  // Filter models based on search
  const filteredModels = provider.models.filter(m =>
    m.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
    m.description.toLowerCase().includes(searchQuery.toLowerCase())
  );

  const handleProviderChange = (providerId: string) => {
    setSelectedProvider(providerId);
    const newProvider = PROVIDERS.find(p => p.id === providerId);
    if (newProvider && newProvider.models.length > 0) {
      // Select first model of new provider
      onChange(newProvider.models[0].id);
    }
  };

  const handleModelSelect = (model: ModelInfo) => {
    onChange(model.id);
    setIsOpen(false);
    setSearchQuery('');
  };

  if (compact) {
    return (
      <CompactSelector
        value={value}
        provider={provider}
        providers={PROVIDERS}
        onProviderChange={handleProviderChange}
        onModelChange={onChange}
      />
    );
  }

  return (
    <div className="space-y-2" ref={dropdownRef}>
      {/* Provider Dropdown */}
      <div>
        <label className="block text-xs mb-1" style={{ color: 'var(--text-muted)' }}>Provider</label>
        <select
          className="w-full px-3 py-2 rounded text-sm"
          style={{
            backgroundColor: 'var(--bg-secondary)',
            border: '1px solid var(--border-default)',
            color: 'var(--text-primary)',
          }}
          value={selectedProvider}
          onChange={(e) => handleProviderChange(e.target.value)}
        >
          {PROVIDERS.map(p => (
            <option key={p.id} value={p.id}>
              {p.icon} {p.name}
            </option>
          ))}
        </select>
      </div>

      {/* Model Selector */}
      <div>
        <label className="block text-xs mb-1" style={{ color: 'var(--text-muted)' }}>Model</label>
        <div className="relative">
          <button
            onClick={() => setIsOpen(!isOpen)}
            className="w-full px-3 py-2 rounded text-sm text-left flex items-center justify-between"
            style={{
              backgroundColor: 'var(--bg-secondary)',
              border: '1px solid var(--border-default)',
              color: 'var(--text-primary)',
            }}
          >
            <div className="flex items-center gap-2 overflow-hidden">
              <span className="truncate">{currentModel?.name || value || 'Select model...'}</span>
              {currentModel && (
                <span 
                  className="text-xs px-1 rounded"
                  style={{ backgroundColor: getTierColor(currentModel.tier), color: 'white' }}
                >
                  {currentModel.tier}
                </span>
              )}
            </div>
            <span style={{ color: 'var(--text-muted)' }}>{isOpen ? '▲' : '▼'}</span>
          </button>

          {isOpen && (
            <div
              className="absolute z-50 w-full mt-1 rounded shadow-lg max-h-80 overflow-hidden"
              style={{
                backgroundColor: 'var(--surface-panel)',
                border: '1px solid var(--border-default)',
              }}
            >
              {/* Search */}
              <div className="p-2" style={{ borderBottom: '1px solid var(--border-default)' }}>
                <input
                  type="text"
                  placeholder="Search models..."
                  value={searchQuery}
                  onChange={(e) => setSearchQuery(e.target.value)}
                  className="w-full px-2 py-1 rounded text-sm"
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-default)',
                    color: 'var(--text-primary)',
                  }}
                  autoFocus
                />
              </div>

              {/* Model List */}
              <div className="overflow-y-auto max-h-60">
                {filteredModels.map(model => (
                  <ModelOption
                    key={model.id}
                    model={model}
                    isSelected={model.id === value}
                    onClick={() => handleModelSelect(model)}
                  />
                ))}
                {filteredModels.length === 0 && (
                  <div className="p-3 text-center text-sm" style={{ color: 'var(--text-muted)' }}>
                    No models found
                  </div>
                )}
              </div>

              {/* Provider Info */}
              <div 
                className="p-2 text-xs"
                style={{ 
                  borderTop: '1px solid var(--border-default)',
                  color: 'var(--text-muted)',
                  backgroundColor: 'var(--bg-secondary)',
                }}
              >
                <span>Requires: </span>
                <code style={{ color: 'var(--accent-primary)' }}>{provider.envVar}</code>
                {provider.envVarAlt && (
                  <span> or <code style={{ color: 'var(--accent-primary)' }}>{provider.envVarAlt}</code></span>
                )}
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Current Model Info */}
      {currentModel && (
        <div 
          className="text-xs p-2 rounded"
          style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-secondary)' }}
        >
          <div className="mb-1">{currentModel.description}</div>
          <div className="flex items-center gap-2 flex-wrap">
            <span title="Context window">📏 {formatContextWindow(currentModel.contextWindow)}</span>
            {currentModel.capabilities.map(cap => (
              <span key={cap} title={cap}>{getCapabilityIcon(cap)}</span>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function ModelOption({ model, isSelected, onClick }: { model: ModelInfo; isSelected: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className="w-full px-3 py-2 text-left hover:opacity-90 transition-colors"
      style={{
        backgroundColor: isSelected ? 'var(--accent-primary)' : 'transparent',
        color: isSelected ? 'white' : 'var(--text-primary)',
      }}
    >
      <div className="flex items-center justify-between">
        <span className="font-medium text-sm">{model.name}</span>
        <span 
          className="text-xs px-1 rounded"
          style={{ 
            backgroundColor: isSelected ? 'rgba(255,255,255,0.2)' : getTierColor(model.tier),
            color: 'white',
          }}
        >
          {model.tier}
        </span>
      </div>
      <div className="text-xs mt-0.5" style={{ color: isSelected ? 'rgba(255,255,255,0.8)' : 'var(--text-muted)' }}>
        {model.description}
      </div>
      <div className="flex items-center gap-1 mt-1 text-xs" style={{ color: isSelected ? 'rgba(255,255,255,0.7)' : 'var(--text-muted)' }}>
        <span>📏 {formatContextWindow(model.contextWindow)}</span>
        {model.capabilities.slice(0, 4).map(cap => (
          <span key={cap}>{getCapabilityIcon(cap)}</span>
        ))}
        {model.capabilities.length > 4 && <span>+{model.capabilities.length - 4}</span>}
      </div>
    </button>
  );
}

// Compact version for sub-agents
function CompactSelector({ 
  value, 
  provider, 
  providers, 
  onProviderChange, 
  onModelChange 
}: { 
  value: string; 
  provider: ProviderInfo; 
  providers: ProviderInfo[];
  onProviderChange: (id: string) => void;
  onModelChange: (model: string) => void;
}) {
  return (
    <div className="flex gap-1">
      <select
        className="px-1 py-1 rounded text-xs"
        style={{
          backgroundColor: 'var(--bg-primary)',
          border: '1px solid var(--border-default)',
          color: 'var(--text-primary)',
          width: '70px',
        }}
        value={provider.id}
        onChange={(e) => onProviderChange(e.target.value)}
      >
        {providers.map(p => (
          <option key={p.id} value={p.id}>{p.icon} {p.id}</option>
        ))}
      </select>
      <select
        className="flex-1 px-1 py-1 rounded text-xs"
        style={{
          backgroundColor: 'var(--bg-primary)',
          border: '1px solid var(--border-default)',
          color: 'var(--text-primary)',
        }}
        value={value}
        onChange={(e) => onModelChange(e.target.value)}
      >
        {provider.models.map(m => (
          <option key={m.id} value={m.id}>{m.name}</option>
        ))}
      </select>
    </div>
  );
}

export default ModelSelector;
