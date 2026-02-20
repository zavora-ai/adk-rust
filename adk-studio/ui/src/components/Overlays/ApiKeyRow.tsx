import React, { useState } from 'react';
import type { ProviderKeyStatus } from '../../hooks/useApiKeys';

export interface ApiKeyRowProps {
  provider: ProviderKeyStatus;
  onSave: (name: string, value: string) => Promise<void>;
  onRemove: (name: string) => Promise<void>;
}

/**
 * Displays a single provider's API key status with icon, name, source badge,
 * masked value, and edit/remove actions. Expands an inline input for entering
 * or updating keys when the row is in "not_set" or "project" state.
 */
export function ApiKeyRow({ provider, onSave, onRemove }: ApiKeyRowProps) {
  const [isEditing, setIsEditing] = useState(false);
  const [keyValue, setKeyValue] = useState('');
  const [isRevealed, setIsRevealed] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [isRemoving, setIsRemoving] = useState(false);

  const isEnvironment = provider.source === 'environment';
  const isProject = provider.source === 'project';
  const isNotSet = provider.source === 'not_set';

  const handleSave = async () => {
    if (!keyValue.trim()) return;
    setIsSaving(true);
    try {
      await onSave(provider.name, keyValue.trim());
      setKeyValue('');
      setIsEditing(false);
      setIsRevealed(false);
    } finally {
      setIsSaving(false);
    }
  };

  const handleRemove = async () => {
    setIsRemoving(true);
    try {
      await onRemove(provider.name);
    } finally {
      setIsRemoving(false);
    }
  };

  const handleCancel = () => {
    setKeyValue('');
    setIsEditing(false);
    setIsRevealed(false);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      handleSave();
    } else if (e.key === 'Escape') {
      handleCancel();
    }
  };

  return (
    <div
      className="p-3 rounded"
      style={{ backgroundColor: 'var(--bg-secondary)' }}
    >
      {/* Provider header row */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="text-base" role="img" aria-label={provider.providerName}>
            {provider.providerIcon}
          </span>
          <div>
            <div className="text-sm font-medium" style={{ color: 'var(--text-primary)' }}>
              {provider.providerName}
            </div>
            <div className="flex items-center gap-2 mt-0.5">
              <code
                className="text-xs font-mono"
                style={{ color: 'var(--text-muted)' }}
              >
                {provider.name}
              </code>
              <SourceBadge source={provider.source} masked={provider.masked} />
            </div>
          </div>
        </div>

        {/* Actions for project keys */}
        {isProject && !isEditing && (
          <div className="flex items-center gap-1">
            <button
              onClick={() => setIsEditing(true)}
              className="px-2 py-1 rounded text-xs transition-colors hover:opacity-80"
              style={{
                backgroundColor: 'var(--bg-primary)',
                color: 'var(--text-secondary)',
                border: '1px solid var(--border-default)',
              }}
            >
              Edit
            </button>
            <button
              onClick={handleRemove}
              disabled={isRemoving}
              className="px-2 py-1 rounded text-xs transition-colors hover:opacity-80"
              style={{
                backgroundColor: 'var(--bg-primary)',
                color: 'var(--accent-error, #ef4444)',
                border: '1px solid var(--border-default)',
                opacity: isRemoving ? 0.5 : 1,
              }}
            >
              {isRemoving ? '...' : 'Remove'}
            </button>
          </div>
        )}
      </div>

      {/* Inline input for not_set state (click to expand) */}
      {isNotSet && !isEditing && (
        <button
          onClick={() => setIsEditing(true)}
          className="mt-2 w-full px-3 py-2 rounded text-xs text-left transition-colors hover:opacity-80"
          style={{
            backgroundColor: 'var(--bg-primary)',
            border: '1px dashed var(--border-default)',
            color: 'var(--text-muted)',
          }}
        >
          Enter key...
        </button>
      )}

      {/* Editing input */}
      {isEditing && (
        <div className="mt-2 flex gap-2">
          <div className="flex-1 relative">
            <input
              type={isRevealed ? 'text' : 'password'}
              value={keyValue}
              onChange={e => setKeyValue(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={`Enter ${provider.name}`}
              autoFocus
              className="w-full px-3 py-2 pr-8 rounded text-sm"
              style={{
                backgroundColor: 'var(--bg-primary)',
                border: '1px solid var(--accent-primary)',
                color: 'var(--text-primary)',
              }}
            />
            <button
              type="button"
              onClick={() => setIsRevealed(!isRevealed)}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-sm"
              style={{ color: 'var(--text-muted)' }}
              aria-label={isRevealed ? 'Hide key' : 'Reveal key'}
            >
              {isRevealed ? '🙈' : '👁️'}
            </button>
          </div>
          <button
            onClick={handleSave}
            disabled={!keyValue.trim() || isSaving}
            className="px-3 py-2 rounded text-xs font-medium transition-colors"
            style={{
              backgroundColor: 'var(--accent-primary)',
              color: 'white',
              opacity: !keyValue.trim() || isSaving ? 0.5 : 1,
            }}
          >
            {isSaving ? '...' : 'Save'}
          </button>
          <button
            onClick={handleCancel}
            className="px-3 py-2 rounded text-xs font-medium transition-colors"
            style={{
              backgroundColor: 'var(--bg-primary)',
              color: 'var(--text-secondary)',
              border: '1px solid var(--border-default)',
            }}
          >
            Cancel
          </button>
        </div>
      )}

      {/* Read-only environment badge note */}
      {isEnvironment && (
        <div
          className="mt-1 text-xs"
          style={{ color: 'var(--text-muted)' }}
        >
          Detected from your environment. To change, update your <code>.env</code> file.
        </div>
      )}

      {/* Override indicator: project key overrides an environment key */}
      {isProject && provider.environmentDetected && (
        <div
          className="mt-1 text-xs flex items-center gap-1"
          style={{ color: 'var(--accent-warning, #f59e0b)' }}
        >
          ⚠️ Overrides environment key
        </div>
      )}
    </div>
  );
}

/**
 * Visual badge indicating the source of an API key.
 */
function SourceBadge({ source, masked }: { source: ProviderKeyStatus['source']; masked: string | null }) {
  switch (source) {
    case 'environment':
      return (
        <span
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs"
          style={{
            backgroundColor: 'rgba(16, 185, 129, 0.15)',
            color: 'var(--accent-success, #10b981)',
          }}
        >
          ✅ from environment
          {masked && <span className="font-mono ml-1">{masked}</span>}
        </span>
      );
    case 'project':
      return (
        <span
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs"
          style={{
            backgroundColor: 'rgba(59, 130, 246, 0.15)',
            color: 'var(--accent-primary)',
          }}
        >
          🔑 project key
          {masked && <span className="font-mono ml-1">{masked}</span>}
        </span>
      );
    case 'not_set':
      return (
        <span
          className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs"
          style={{
            backgroundColor: 'rgba(107, 114, 128, 0.15)',
            color: 'var(--text-muted)',
          }}
        >
          ⚪ not set
        </span>
      );
  }
}

export default ApiKeyRow;
