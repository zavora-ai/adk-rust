/**
 * TriggerPanel Component for ADK Studio
 * 
 * Properties panel for configuring Trigger action nodes.
 * Provides UI for selecting trigger type and configuring type-specific options.
 * Settings are organized by trigger type - only relevant settings are shown.
 * 
 * Requirements: 2.1, 2.2, 2.3, 12.2
 */

import React, { useCallback, useState } from 'react';
import { StandardPropertiesPanel } from './StandardPropertiesPanel';
import { Tooltip, ACTION_NODE_TOOLTIPS } from '../Overlays/Tooltip';
import type { 
  TriggerNodeConfig, 
  TriggerType, 
  WebhookConfig, 
  ScheduleConfig, 
  EventConfig,
  WebhookMethod,
  WebhookAuth,
  ManualTriggerConfig,
} from '../../types/actionNodes';
import { DEFAULT_MANUAL_TRIGGER_CONFIG } from '../../types/actionNodes';
import type { StandardProperties } from '../../types/standardProperties';
import '../../styles/triggerPanel.css';

// ============================================
// Constants
// ============================================

const TRIGGER_TYPES: readonly TriggerType[] = ['manual', 'webhook', 'schedule', 'event'];

const TRIGGER_TYPE_INFO: Record<TriggerType, { label: string; icon: string; description: string }> = {
  manual: {
    label: 'Manual',
    icon: '👆',
    description: 'Triggered by user action (Run button or API)',
  },
  webhook: {
    label: 'Webhook',
    icon: '🌐',
    description: 'Triggered by HTTP request',
  },
  schedule: {
    label: 'Schedule',
    icon: '⏰',
    description: 'Triggered on a cron schedule',
  },
  event: {
    label: 'Event',
    icon: '⚡',
    description: 'Triggered by external events',
  },
};

const WEBHOOK_METHODS: readonly WebhookMethod[] = ['GET', 'POST'];

const WEBHOOK_AUTH_TYPES: readonly WebhookAuth[] = ['none', 'bearer', 'api_key'];

const WEBHOOK_AUTH_LABELS: Record<WebhookAuth, string> = {
  none: 'No Authentication',
  bearer: 'Bearer Token',
  api_key: 'API Key',
};

// Common timezones for schedule configuration
const COMMON_TIMEZONES = [
  'UTC',
  'America/New_York',
  'America/Chicago',
  'America/Denver',
  'America/Los_Angeles',
  'Europe/London',
  'Europe/Paris',
  'Europe/Berlin',
  'Asia/Tokyo',
  'Asia/Shanghai',
  'Asia/Singapore',
  'Australia/Sydney',
] as const;

// ============================================
// Helper Components
// ============================================

interface CollapsibleSectionProps {
  title: string;
  defaultOpen?: boolean;
  children: React.ReactNode;
}

function CollapsibleSection({ title, defaultOpen = true, children }: CollapsibleSectionProps) {
  const [isOpen, setIsOpen] = useState(defaultOpen);
  
  return (
    <div className="trigger-panel-section">
      <button 
        className="trigger-panel-section-header"
        onClick={() => setIsOpen(!isOpen)}
        type="button"
      >
        <span className="trigger-panel-section-toggle">{isOpen ? '▼' : '▶'}</span>
        <span className="trigger-panel-section-title">{title}</span>
      </button>
      {isOpen && (
        <div className="trigger-panel-section-content">
          {children}
        </div>
      )}
    </div>
  );
}

interface FieldProps {
  label: string;
  hint?: string;
  required?: boolean;
  tooltip?: string;
  children: React.ReactNode;
}

function Field({ label, hint, required, tooltip, children }: FieldProps) {
  const labelContent = (
    <label className="trigger-panel-label">
      {label}
      {required && <span className="trigger-panel-required">*</span>}
      {hint && <span className="trigger-panel-hint">({hint})</span>}
      {tooltip && <span className="trigger-panel-tooltip-icon" title={tooltip}>ℹ️</span>}
    </label>
  );

  return (
    <div className="trigger-panel-field">
      {tooltip ? (
        <Tooltip content={tooltip} position="right" delay={300}>
          {labelContent}
        </Tooltip>
      ) : (
        labelContent
      )}
      {children}
    </div>
  );
}

// ============================================
// Main Component
// ============================================

export interface TriggerPanelProps {
  /** Current trigger node configuration */
  node: TriggerNodeConfig;
  /** Callback when configuration changes */
  onChange: (node: TriggerNodeConfig) => void;
}

/**
 * TriggerPanel provides configuration UI for Trigger action nodes.
 * 
 * Settings are organized by trigger type:
 * - Manual: Input label, default prompt
 * - Webhook: Path, method, authentication
 * - Schedule: Cron expression, timezone
 * - Event: Source, event type, filters
 * 
 * @see Requirements 2.1, 2.2, 2.3, 12.2
 */
export function TriggerPanel({ node, onChange }: TriggerPanelProps) {
  
  // ============================================
  // Update Handlers
  // ============================================
  
  const updateTriggerType = useCallback((triggerType: TriggerType) => {
    const updates: Partial<TriggerNodeConfig> = { triggerType };
    
    // Initialize type-specific config if not present
    if (triggerType === 'manual' && !node.manual) {
      updates.manual = { ...DEFAULT_MANUAL_TRIGGER_CONFIG };
    } else if (triggerType === 'webhook' && !node.webhook) {
      updates.webhook = {
        path: '/api/webhook/trigger',
        method: 'POST',
        auth: 'none',
      };
    } else if (triggerType === 'schedule' && !node.schedule) {
      updates.schedule = {
        cron: '0 * * * *', // Every hour
        timezone: 'UTC',
      };
    } else if (triggerType === 'event' && !node.event) {
      updates.event = {
        source: '',
        eventType: '',
      };
    }
    
    onChange({ ...node, ...updates });
  }, [node, onChange]);
  
  const updateWebhook = useCallback((updates: Partial<WebhookConfig>) => {
    onChange({
      ...node,
      webhook: { ...node.webhook!, ...updates },
    });
  }, [node, onChange]);
  
  const updateSchedule = useCallback((updates: Partial<ScheduleConfig>) => {
    onChange({
      ...node,
      schedule: { ...node.schedule!, ...updates },
    });
  }, [node, onChange]);
  
  const updateEvent = useCallback((updates: Partial<EventConfig>) => {
    onChange({
      ...node,
      event: { ...node.event!, ...updates },
    });
  }, [node, onChange]);
  
  const updateManual = useCallback((updates: Partial<ManualTriggerConfig>) => {
    onChange({
      ...node,
      manual: { 
        ...DEFAULT_MANUAL_TRIGGER_CONFIG,
        ...node.manual, 
        ...updates 
      },
    });
  }, [node, onChange]);
  
  const updateStandardProperties = useCallback((props: StandardProperties) => {
    onChange({ ...node, ...props });
  }, [node, onChange]);
  
  // ============================================
  // Render
  // ============================================
  
  const currentType = TRIGGER_TYPE_INFO[node.triggerType];
  
  return (
    <div className="trigger-panel">
      {/* Trigger Type Selector - Compact tabs */}
      <div className="trigger-type-tabs">
        {TRIGGER_TYPES.map((type) => {
          const info = TRIGGER_TYPE_INFO[type];
          return (
            <button
              key={type}
              type="button"
              className={`trigger-type-tab ${node.triggerType === type ? 'selected' : ''}`}
              onClick={() => updateTriggerType(type)}
              title={info.description}
            >
              <span className="trigger-type-tab-icon">{info.icon}</span>
              <span className="trigger-type-tab-label">{info.label}</span>
            </button>
          );
        })}
      </div>
      
      {/* Current type description */}
      <div className="trigger-type-description">
        <span className="trigger-type-description-icon">{currentType.icon}</span>
        <span className="trigger-type-description-text">{currentType.description}</span>
      </div>

      {/* ============================================ */}
      {/* MANUAL TRIGGER SETTINGS */}
      {/* ============================================ */}
      {node.triggerType === 'manual' && (
        <CollapsibleSection title="Manual Trigger Settings" defaultOpen>
          <Field 
            label="Input Label" 
            hint="shown above chat input"
            tooltip="The label displayed above the chat input field when the workflow is ready to receive input"
          >
            <input
              type="text"
              className="trigger-panel-input"
              value={node.manual?.inputLabel || DEFAULT_MANUAL_TRIGGER_CONFIG.inputLabel}
              onChange={(e) => updateManual({ inputLabel: e.target.value })}
              placeholder="e.g., Enter your question"
            />
          </Field>
          
          <Field 
            label="Default Prompt" 
            hint="placeholder text"
            tooltip="Placeholder text shown in the chat input field to guide users on what to enter"
          >
            <textarea
              className="trigger-panel-textarea"
              value={node.manual?.defaultPrompt ?? DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt}
              onChange={(e) => updateManual({ defaultPrompt: e.target.value })}
              onFocus={(e) => {
                if (e.target.value === DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt) {
                  updateManual({ defaultPrompt: '' });
                }
              }}
              placeholder="Placeholder text for input field"
              rows={3}
            />
          </Field>
          
          {/* Preview of how it will appear */}
          <div className="trigger-panel-preview">
            <span className="trigger-panel-preview-label">Preview:</span>
            <div className="trigger-panel-manual-preview">
              <span className="trigger-panel-manual-preview-label">
                {node.manual?.inputLabel || DEFAULT_MANUAL_TRIGGER_CONFIG.inputLabel}
              </span>
              <span className="trigger-panel-manual-preview-placeholder">
                {node.manual?.defaultPrompt || DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt}
              </span>
            </div>
          </div>
          
          <div className="trigger-panel-info">
            <span className="trigger-panel-info-icon">ℹ️</span>
            <span className="trigger-panel-info-text">
              Click the <strong>Run</strong> button or use the API to start this workflow.
            </span>
          </div>
        </CollapsibleSection>
      )}

      {/* ============================================ */}
      {/* WEBHOOK SETTINGS */}
      {/* ============================================ */}
      {node.triggerType === 'webhook' && node.webhook && (
        <>
          <CollapsibleSection title="Endpoint Configuration" defaultOpen>
            <Field label="Webhook Path" required hint="e.g., /api/webhook/my-flow" tooltip={ACTION_NODE_TOOLTIPS.webhookPath}>
              <input
                type="text"
                className="trigger-panel-input"
                value={node.webhook.path}
                onChange={(e) => updateWebhook({ path: e.target.value })}
                placeholder="/api/webhook/trigger"
              />
            </Field>
            
            <Field label="HTTP Method" required tooltip={ACTION_NODE_TOOLTIPS.webhookMethod}>
              <div className="trigger-panel-button-group">
                {WEBHOOK_METHODS.map((method) => (
                  <button
                    key={method}
                    type="button"
                    className={`trigger-panel-button ${node.webhook?.method === method ? 'selected' : ''}`}
                    onClick={() => updateWebhook({ method })}
                  >
                    {method}
                  </button>
                ))}
              </div>
            </Field>
            
            {/* Webhook URL Preview */}
            <div className="trigger-panel-preview">
              <span className="trigger-panel-preview-label">Endpoint:</span>
              <code className="trigger-panel-preview-value">
                {node.webhook.method} {node.webhook.path}
              </code>
            </div>
          </CollapsibleSection>
          
          <CollapsibleSection title="Authentication" defaultOpen={node.webhook.auth !== 'none'}>
            <Field label="Auth Type" tooltip={ACTION_NODE_TOOLTIPS.webhookAuth}>
              <select
                className="trigger-panel-select"
                value={node.webhook.auth}
                onChange={(e) => updateWebhook({ auth: e.target.value as WebhookAuth })}
              >
                {WEBHOOK_AUTH_TYPES.map((auth) => (
                  <option key={auth} value={auth}>
                    {WEBHOOK_AUTH_LABELS[auth]}
                  </option>
                ))}
              </select>
            </Field>
            
            {/* Bearer Token Configuration */}
            {node.webhook.auth === 'bearer' && (
              <Field label="Token Environment Variable" hint="env var name">
                <input
                  type="text"
                  className="trigger-panel-input"
                  value={node.webhook.authConfig?.tokenEnvVar || ''}
                  onChange={(e) => updateWebhook({ 
                    authConfig: { ...node.webhook?.authConfig, tokenEnvVar: e.target.value }
                  })}
                  placeholder="WEBHOOK_TOKEN"
                />
              </Field>
            )}
            
            {/* API Key Configuration */}
            {node.webhook.auth === 'api_key' && (
              <>
                <Field label="Header Name" hint="e.g., X-API-Key">
                  <input
                    type="text"
                    className="trigger-panel-input"
                    value={node.webhook.authConfig?.headerName || ''}
                    onChange={(e) => updateWebhook({ 
                      authConfig: { ...node.webhook?.authConfig, headerName: e.target.value }
                    })}
                    placeholder="X-API-Key"
                  />
                </Field>
                <Field label="API Key Environment Variable" hint="env var name">
                  <input
                    type="text"
                    className="trigger-panel-input"
                    value={node.webhook.authConfig?.tokenEnvVar || ''}
                    onChange={(e) => updateWebhook({ 
                      authConfig: { ...node.webhook?.authConfig, tokenEnvVar: e.target.value }
                    })}
                    placeholder="API_KEY"
                  />
                </Field>
              </>
            )}
            
            {node.webhook.auth === 'none' && (
              <div className="trigger-panel-info">
                <span className="trigger-panel-info-icon">⚠️</span>
                <span className="trigger-panel-info-text">
                  No authentication. Anyone with the URL can trigger this workflow.
                </span>
              </div>
            )}
          </CollapsibleSection>
        </>
      )}

      {/* ============================================ */}
      {/* SCHEDULE SETTINGS */}
      {/* ============================================ */}
      {node.triggerType === 'schedule' && node.schedule && (
        <>
          <CollapsibleSection title="Schedule Configuration" defaultOpen>
            <ScheduleSection 
              schedule={node.schedule} 
              onChange={updateSchedule} 
            />
          </CollapsibleSection>
          
          <CollapsibleSection title="Trigger Input" defaultOpen>
            <Field 
              label="Default Prompt" 
              hint="sent when schedule fires"
              tooltip="The prompt/input that will be sent to the workflow when the schedule triggers"
            >
              <textarea
                className="trigger-panel-textarea"
                value={node.schedule.defaultPrompt || ''}
                onChange={(e) => updateSchedule({ defaultPrompt: e.target.value })}
                placeholder="e.g., Generate daily report for {{date}}"
                rows={3}
              />
            </Field>
            <div className="trigger-panel-info">
              <span className="trigger-panel-info-icon">ℹ️</span>
              <span className="trigger-panel-info-text">
                This prompt will be sent to the first agent when the schedule fires.
                Leave empty to send schedule metadata (cron, timezone, timestamp).
              </span>
            </div>
          </CollapsibleSection>
        </>
      )}

      {/* ============================================ */}
      {/* EVENT SETTINGS */}
      {/* ============================================ */}
      {node.triggerType === 'event' && node.event && (
        <>
          <CollapsibleSection title="Event Source" defaultOpen>
            <Field label="Source Identifier" required hint="unique identifier">
              <input
                type="text"
                className="trigger-panel-input"
                value={node.event.source}
                onChange={(e) => updateEvent({ source: e.target.value })}
                placeholder="my-event-source"
              />
            </Field>
            
            <Field label="Event Type" hint="filter by type">
              <input
                type="text"
                className="trigger-panel-input"
                value={node.event.eventType}
                onChange={(e) => updateEvent({ eventType: e.target.value })}
                placeholder="user.created"
              />
            </Field>
          </CollapsibleSection>
          
          <CollapsibleSection title="Event Filters" defaultOpen={false}>
            <div className="trigger-panel-info">
              <span className="trigger-panel-info-icon">ℹ️</span>
              <span className="trigger-panel-info-text">
                Event filters allow you to only trigger on events matching specific criteria.
                Use JSONPath expressions to filter event payloads.
              </span>
            </div>
            
            <Field label="Filter Expression" hint="JSONPath">
              <input
                type="text"
                className="trigger-panel-input trigger-panel-input-mono"
                value={node.event.filter || ''}
                onChange={(e) => updateEvent({ filter: e.target.value })}
                placeholder="$.data.status == 'active'"
              />
            </Field>
          </CollapsibleSection>
        </>
      )}
      
      {/* Standard Properties - Always at bottom */}
      <StandardPropertiesPanel
        properties={node}
        onChange={updateStandardProperties}
        showIdentity
      />
    </div>
  );
}

// ============================================
// Schedule Section Component
// ============================================

interface ScheduleSectionProps {
  schedule: ScheduleConfig;
  onChange: (updates: Partial<ScheduleConfig>) => void;
}

/**
 * Schedule configuration section with cron expression input,
 * timezone selector, and human-readable preview.
 * 
 * @see Requirement 2.3
 */
function ScheduleSection({ schedule, onChange }: ScheduleSectionProps) {
  const [cronError, setCronError] = useState<string | null>(null);
  
  // Validate cron expression (basic validation)
  const validateCron = (cron: string): boolean => {
    const parts = cron.trim().split(/\s+/);
    if (parts.length < 5 || parts.length > 6) {
      setCronError('Cron expression must have 5 or 6 fields');
      return false;
    }
    setCronError(null);
    return true;
  };
  
  const handleCronChange = (cron: string) => {
    validateCron(cron);
    onChange({ cron });
  };
  
  // Generate human-readable description of cron expression
  const getCronDescription = (cron: string): string => {
    const parts = cron.trim().split(/\s+/);
    if (parts.length < 5) return 'Invalid cron expression';
    
    const [minute, hour, dayOfMonth, month, dayOfWeek] = parts;
    
    // Simple descriptions for common patterns
    if (cron === '* * * * *') return 'Every minute';
    if (cron === '0 * * * *') return 'Every hour';
    if (cron === '0 0 * * *') return 'Every day at midnight';
    if (cron === '0 0 * * 0') return 'Every Sunday at midnight';
    if (cron === '0 0 1 * *') return 'First day of every month at midnight';
    
    // Build description
    const desc: string[] = [];
    
    if (minute === '*') {
      desc.push('Every minute');
    } else if (minute.includes('/')) {
      desc.push(`Every ${minute.split('/')[1]} minutes`);
    } else {
      desc.push(`At minute ${minute}`);
    }
    
    if (hour !== '*') {
      if (hour.includes('/')) {
        desc.push(`every ${hour.split('/')[1]} hours`);
      } else {
        desc.push(`at ${hour}:00`);
      }
    }
    
    if (dayOfMonth !== '*') {
      desc.push(`on day ${dayOfMonth}`);
    }
    
    if (month !== '*') {
      desc.push(`in month ${month}`);
    }
    
    if (dayOfWeek !== '*') {
      const days = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];
      const dayNum = parseInt(dayOfWeek, 10);
      if (!isNaN(dayNum) && dayNum >= 0 && dayNum <= 6) {
        desc.push(`on ${days[dayNum]}`);
      }
    }
    
    return desc.join(' ') || 'Custom schedule';
  };
  
  return (
    <>
      <Field label="Cron Expression" required hint="minute hour day month weekday" tooltip={ACTION_NODE_TOOLTIPS.cronExpression}>
        <input
          type="text"
          className={`trigger-panel-input trigger-panel-input-mono ${cronError ? 'error' : ''}`}
          value={schedule.cron}
          onChange={(e) => handleCronChange(e.target.value)}
          placeholder="0 * * * *"
        />
        {cronError && (
          <span className="trigger-panel-error">{cronError}</span>
        )}
      </Field>
      
      {/* Common cron presets */}
      <div className="trigger-panel-presets">
        <span className="trigger-panel-presets-label">Presets:</span>
        <div className="trigger-panel-presets-buttons">
          <button type="button" onClick={() => onChange({ cron: '* * * * *' })}>Every minute</button>
          <button type="button" onClick={() => onChange({ cron: '0 * * * *' })}>Hourly</button>
          <button type="button" onClick={() => onChange({ cron: '0 0 * * *' })}>Daily</button>
          <button type="button" onClick={() => onChange({ cron: '0 0 * * 0' })}>Weekly</button>
          <button type="button" onClick={() => onChange({ cron: '0 0 1 * *' })}>Monthly</button>
        </div>
      </div>
      
      <Field label="Timezone" tooltip={ACTION_NODE_TOOLTIPS.timezone}>
        <select
          className="trigger-panel-select"
          value={schedule.timezone}
          onChange={(e) => onChange({ timezone: e.target.value })}
        >
          {COMMON_TIMEZONES.map((tz) => (
            <option key={tz} value={tz}>{tz}</option>
          ))}
        </select>
      </Field>
      
      {/* Human-readable preview */}
      <div className="trigger-panel-preview">
        <span className="trigger-panel-preview-label">Schedule:</span>
        <span className="trigger-panel-preview-value">
          {getCronDescription(schedule.cron)}
        </span>
        <span className="trigger-panel-preview-tz">
          ({schedule.timezone})
        </span>
      </div>
    </>
  );
}

export default TriggerPanel;
