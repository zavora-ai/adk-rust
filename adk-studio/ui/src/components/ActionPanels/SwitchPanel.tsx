/**
 * SwitchPanel Component for ADK Studio
 * 
 * Properties panel for configuring Switch action nodes.
 * Provides UI for evaluation mode, condition builder, expression mode,
 * and default branch configuration.
 * 
 * Requirements: 6.1, 6.2, 6.3, 12.2
 */

import { useCallback, useState } from 'react';
import { StandardPropertiesPanel } from './StandardPropertiesPanel';
import { CollapsibleSection } from '../shared/CollapsibleSection';
import { Field } from '../shared/Field';
import type { 
  SwitchNodeConfig, 
  EvaluationMode,
  SwitchCondition,
  ConditionOperator,
  ExpressionMode,
} from '../../types/actionNodes';
import type { StandardProperties } from '../../types/standardProperties';
import '../../styles/switchPanel.css';

// ============================================
// Constants
// ============================================

const EVALUATION_MODES: readonly EvaluationMode[] = ['first_match', 'all_match'];

const EVALUATION_MODE_CONFIG: Record<EvaluationMode, {
  label: string;
  description: string;
  icon: string;
}> = {
  first_match: {
    label: 'First Match',
    description: 'Stop at first matching condition',
    icon: '1️⃣',
  },
  all_match: {
    label: 'All Match (Fan Out)',
    description: 'Execute all branches in parallel',
    icon: '🔀',
  },
};

const CONDITION_OPERATORS: readonly ConditionOperator[] = [
  'eq', 'neq', 'gt', 'lt', 'gte', 'lte',
  'contains', 'startsWith', 'endsWith',
  'matches', 'in', 'empty', 'exists',
];

const OPERATOR_CONFIG: Record<ConditionOperator, {
  label: string;
  description: string;
  requiresValue: boolean;
}> = {
  eq: { label: '=', description: 'Equals', requiresValue: true },
  neq: { label: '≠', description: 'Not equals', requiresValue: true },
  gt: { label: '>', description: 'Greater than', requiresValue: true },
  lt: { label: '<', description: 'Less than', requiresValue: true },
  gte: { label: '≥', description: 'Greater or equal', requiresValue: true },
  lte: { label: '≤', description: 'Less or equal', requiresValue: true },
  contains: { label: 'contains', description: 'Contains substring', requiresValue: true },
  startsWith: { label: 'starts with', description: 'Starts with', requiresValue: true },
  endsWith: { label: 'ends with', description: 'Ends with', requiresValue: true },
  matches: { label: 'matches', description: 'Matches regex', requiresValue: true },
  in: { label: 'in', description: 'In array', requiresValue: true },
  empty: { label: 'is empty', description: 'Is empty/null', requiresValue: false },
  exists: { label: 'exists', description: 'Field exists', requiresValue: false },
};

// ============================================
// Main Component
// ============================================

export interface SwitchPanelProps {
  /** Current Switch node configuration */
  node: SwitchNodeConfig;
  /** Callback when configuration changes */
  onChange: (node: SwitchNodeConfig) => void;
}

/**
 * SwitchPanel provides configuration UI for Switch action nodes.
 * 
 * Features:
 * - Evaluation mode selector (first_match/all_match) (Requirement 6.2)
 * - Condition builder with add/remove (Requirement 6.1)
 * - Field, operator, value inputs for each condition (Requirement 6.1)
 * - Output port assignment per condition (Requirement 6.1)
 * - Expression mode toggle with JavaScript editor (Requirement 6.3)
 * - Default branch configuration
 * - Standard properties panel integration
 * 
 * @see Requirements 6.1, 6.2, 6.3, 12.2
 */
export function SwitchPanel({ node, onChange }: SwitchPanelProps) {
  
  // ============================================
  // Update Handlers
  // ============================================
  
  const updateEvaluationMode = useCallback((evaluationMode: EvaluationMode) => {
    onChange({ ...node, evaluationMode });
  }, [node, onChange]);
  
  const updateConditions = useCallback((conditions: SwitchCondition[]) => {
    onChange({ ...node, conditions });
  }, [node, onChange]);
  
  const updateDefaultBranch = useCallback((defaultBranch: string | undefined) => {
    if (defaultBranch === undefined || defaultBranch === '') {
      const { defaultBranch: _, ...rest } = node;
      onChange(rest as SwitchNodeConfig);
    } else {
      onChange({ ...node, defaultBranch });
    }
  }, [node, onChange]);
  
  const updateExpressionMode = useCallback((expressionMode: ExpressionMode | undefined) => {
    if (expressionMode === undefined) {
      const { expressionMode: _, ...rest } = node;
      onChange(rest as SwitchNodeConfig);
    } else {
      onChange({ ...node, expressionMode });
    }
  }, [node, onChange]);
  
  const updateStandardProperties = useCallback((props: StandardProperties) => {
    onChange({ ...node, ...props });
  }, [node, onChange]);
  
  const isExpressionMode = node.expressionMode?.enabled ?? false;
  
  // ============================================
  // Render
  // ============================================
  
  return (
    <div className="switch-panel">
      {/* Evaluation Mode Section (Requirement 6.2) */}
      <CollapsibleSection title="Evaluation Mode" defaultOpen>
        <EvaluationModeSection
          mode={node.evaluationMode}
          onChange={updateEvaluationMode}
        />
      </CollapsibleSection>
      
      {/* Mode Toggle: Conditions vs Expression */}
      <CollapsibleSection title="Routing Mode" defaultOpen>
        <ModeToggleSection
          isExpressionMode={isExpressionMode}
          onToggle={(enabled) => {
            if (enabled) {
              updateExpressionMode({ enabled: true, expression: '' });
            } else {
              updateExpressionMode(undefined);
            }
          }}
        />
      </CollapsibleSection>
      
      {/* Condition Builder (Requirement 6.1) */}
      {!isExpressionMode && (
        <CollapsibleSection title="Conditions" defaultOpen>
          <ConditionBuilderSection
            conditions={node.conditions}
            onChange={updateConditions}
          />
        </CollapsibleSection>
      )}
      
      {/* Expression Mode (Requirement 6.3) */}
      {isExpressionMode && (
        <CollapsibleSection title="Expression" defaultOpen>
          <ExpressionModeSection
            expressionMode={node.expressionMode!}
            onChange={updateExpressionMode}
          />
        </CollapsibleSection>
      )}
      
      {/* Default Branch */}
      <CollapsibleSection title="Default Branch" defaultOpen={false}>
        <DefaultBranchSection
          defaultBranch={node.defaultBranch}
          onChange={updateDefaultBranch}
        />
      </CollapsibleSection>
      
      {/* Standard Properties */}
      <StandardPropertiesPanel
        properties={node}
        onChange={updateStandardProperties}
        showIdentity
      />
    </div>
  );
}

// ============================================
// Evaluation Mode Section Component
// ============================================

interface EvaluationModeSectionProps {
  mode: EvaluationMode;
  onChange: (mode: EvaluationMode) => void;
}

/**
 * Evaluation mode selector section.
 * @see Requirement 6.2
 */
function EvaluationModeSection({ mode, onChange }: EvaluationModeSectionProps) {
  return (
    <div className="switch-evaluation-mode-selector">
      {EVALUATION_MODES.map((m) => {
        const config = EVALUATION_MODE_CONFIG[m];
        return (
          <button
            key={m}
            type="button"
            className={`switch-evaluation-mode-option ${mode === m ? 'selected' : ''}`}
            onClick={() => onChange(m)}
          >
            <span className="switch-evaluation-mode-icon">{config.icon}</span>
            <span className="switch-evaluation-mode-label">{config.label}</span>
            <span className="switch-evaluation-mode-description">{config.description}</span>
          </button>
        );
      })}
    </div>
  );
}

// ============================================
// Mode Toggle Section Component
// ============================================

interface ModeToggleSectionProps {
  isExpressionMode: boolean;
  onToggle: (enabled: boolean) => void;
}

/**
 * Toggle between condition builder and expression mode.
 */
function ModeToggleSection({ isExpressionMode, onToggle }: ModeToggleSectionProps) {
  return (
    <div className="switch-mode-toggle">
      <button
        type="button"
        className={`switch-mode-option ${!isExpressionMode ? 'selected' : ''}`}
        onClick={() => onToggle(false)}
      >
        <span className="switch-mode-icon">🔧</span>
        <span className="switch-mode-label">Condition Builder</span>
        <span className="switch-mode-hint">Visual condition editor</span>
      </button>
      <button
        type="button"
        className={`switch-mode-option ${isExpressionMode ? 'selected' : ''}`}
        onClick={() => onToggle(true)}
      >
        <span className="switch-mode-icon">📜</span>
        <span className="switch-mode-label">Expression</span>
        <span className="switch-mode-hint">JavaScript expression</span>
      </button>
    </div>
  );
}

// ============================================
// Condition Builder Section Component
// ============================================

interface ConditionBuilderSectionProps {
  conditions: SwitchCondition[];
  onChange: (conditions: SwitchCondition[]) => void;
}

/**
 * Condition builder with add/remove functionality.
 * @see Requirement 6.1
 */
function ConditionBuilderSection({ conditions, onChange }: ConditionBuilderSectionProps) {
  
  const handleAdd = () => {
    const newCondition: SwitchCondition = {
      id: `cond_${Date.now()}`,
      name: `Condition ${conditions.length + 1}`,
      field: '',
      operator: 'eq',
      value: '',
      outputPort: `branch_${conditions.length + 1}`,
    };
    onChange([...conditions, newCondition]);
  };
  
  const handleRemove = (index: number) => {
    const newConditions = [...conditions];
    newConditions.splice(index, 1);
    onChange(newConditions);
  };
  
  const handleUpdate = (index: number, updates: Partial<SwitchCondition>) => {
    const newConditions = [...conditions];
    newConditions[index] = { ...newConditions[index], ...updates };
    onChange(newConditions);
  };
  
  const handleMoveUp = (index: number) => {
    if (index === 0) return;
    const newConditions = [...conditions];
    [newConditions[index - 1], newConditions[index]] = [newConditions[index], newConditions[index - 1]];
    onChange(newConditions);
  };
  
  const handleMoveDown = (index: number) => {
    if (index === conditions.length - 1) return;
    const newConditions = [...conditions];
    [newConditions[index], newConditions[index + 1]] = [newConditions[index + 1], newConditions[index]];
    onChange(newConditions);
  };
  
  return (
    <div className="switch-condition-builder">
      {conditions.length === 0 ? (
        <div className="switch-panel-empty">
          <span className="switch-panel-empty-icon">🔀</span>
          <span className="switch-panel-empty-text">No conditions defined</span>
          <span className="switch-panel-empty-hint">Add conditions to create routing branches</span>
        </div>
      ) : (
        <div className="switch-conditions-list">
          {conditions.map((condition, index) => (
            <ConditionRow
              key={condition.id}
              condition={condition}
              index={index}
              total={conditions.length}
              onUpdate={(updates) => handleUpdate(index, updates)}
              onRemove={() => handleRemove(index)}
              onMoveUp={() => handleMoveUp(index)}
              onMoveDown={() => handleMoveDown(index)}
            />
          ))}
        </div>
      )}
      
      <button
        type="button"
        className="switch-condition-add"
        onClick={handleAdd}
      >
        + Add Condition
      </button>
    </div>
  );
}

// ============================================
// Condition Row Component
// ============================================

interface ConditionRowProps {
  condition: SwitchCondition;
  index: number;
  total: number;
  onUpdate: (updates: Partial<SwitchCondition>) => void;
  onRemove: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
}

/**
 * Single condition row with field, operator, value, and output port.
 * @see Requirement 6.1
 */
function ConditionRow({
  condition,
  index,
  total,
  onUpdate,
  onRemove,
  onMoveUp,
  onMoveDown,
}: ConditionRowProps) {
  const [isExpanded, setIsExpanded] = useState(true);
  const operatorConfig = OPERATOR_CONFIG[condition.operator] ?? { label: condition.operator, description: condition.operator, requiresValue: true };
  
  return (
    <div className={`switch-condition-row ${isExpanded ? 'expanded' : 'collapsed'}`}>
      <div className="switch-condition-header">
        <button
          type="button"
          className="switch-condition-toggle"
          onClick={() => setIsExpanded(!isExpanded)}
        >
          {isExpanded ? '▼' : '▶'}
        </button>
        
        <span className="switch-condition-index">#{index + 1}</span>
        
        <input
          type="text"
          className="switch-condition-name-input"
          value={condition.name}
          onChange={(e) => onUpdate({ name: e.target.value })}
          placeholder="Condition name"
        />
        
        <div className="switch-condition-actions">
          <button
            type="button"
            className="switch-condition-action"
            onClick={onMoveUp}
            disabled={index === 0}
            title="Move up"
          >
            ↑
          </button>
          <button
            type="button"
            className="switch-condition-action"
            onClick={onMoveDown}
            disabled={index === total - 1}
            title="Move down"
          >
            ↓
          </button>
          <button
            type="button"
            className="switch-condition-remove"
            onClick={onRemove}
            title="Remove condition"
          >
            ×
          </button>
        </div>
      </div>
      
      {isExpanded && (
        <div className="switch-condition-body">
          {/* Field Input */}
          <Field label="Field" required hint="state path">
            <input
              type="text"
              className="switch-panel-input switch-panel-input-mono"
              value={condition.field}
              onChange={(e) => onUpdate({ field: e.target.value })}
              placeholder="state.field.path"
            />
          </Field>
          
          {/* Operator Selector */}
          <Field label="Operator" required>
            <select
              className="switch-panel-select"
              value={condition.operator}
              onChange={(e) => onUpdate({ operator: e.target.value as ConditionOperator })}
            >
              {CONDITION_OPERATORS.map((op) => (
                <option key={op} value={op}>
                  {OPERATOR_CONFIG[op].label} ({OPERATOR_CONFIG[op].description})
                </option>
              ))}
            </select>
          </Field>
          
          {/* Value Input (conditional) */}
          {operatorConfig.requiresValue && (
            <Field label="Value" required hint="or {{variable}}">
              <input
                type="text"
                className="switch-panel-input"
                value={condition.value !== undefined ? String(condition.value) : ''}
                onChange={(e) => {
                  // Try to parse as number or boolean
                  const val = e.target.value;
                  let parsed: unknown = val;
                  if (val === 'true') parsed = true;
                  else if (val === 'false') parsed = false;
                  else if (!isNaN(Number(val)) && val !== '') parsed = Number(val);
                  onUpdate({ value: parsed });
                }}
                placeholder="Value to compare"
              />
            </Field>
          )}
          
          {/* Output Port */}
          <Field label="Output Port" required hint="branch identifier">
            <input
              type="text"
              className="switch-panel-input"
              value={condition.outputPort}
              onChange={(e) => onUpdate({ outputPort: e.target.value })}
              placeholder="branch_name"
            />
            <div className="switch-panel-field-help">
              This identifier is used to connect edges from this branch.
            </div>
          </Field>
        </div>
      )}
    </div>
  );
}

// ============================================
// Expression Mode Section Component
// ============================================

interface ExpressionModeSectionProps {
  expressionMode: ExpressionMode;
  onChange: (expressionMode: ExpressionMode | undefined) => void;
}

/**
 * Expression mode configuration with JavaScript editor.
 * @see Requirement 6.3
 */
function ExpressionModeSection({ expressionMode, onChange }: ExpressionModeSectionProps) {
  return (
    <div className="switch-expression-section">
      <Field label="JavaScript Expression" required hint="returns branch name">
        <textarea
          className="switch-panel-expression"
          value={expressionMode.expression}
          onChange={(e) => onChange({ ...expressionMode, expression: e.target.value })}
          placeholder={`// Return the branch name to route to
// Available: state (full workflow state)

if (state.score > 80) {
  return 'high';
} else if (state.score > 50) {
  return 'medium';
} else {
  return 'low';
}`}
          rows={10}
          spellCheck={false}
        />
      </Field>
      
      {/* Expression hints */}
      <div className="switch-expression-hints">
        <div className="switch-hint-box">
          <div className="switch-hint-title">Expression Context</div>
          <ul className="switch-hint-list">
            <li><code>state</code> - Full workflow state object</li>
            <li><code>return 'branchName'</code> - Route to named branch</li>
            <li>Standard JavaScript syntax supported</li>
            <li>Runs in sandboxed environment</li>
          </ul>
        </div>
      </div>
      
      <div className="switch-panel-info">
        <span className="switch-panel-info-icon">ℹ️</span>
        <span className="switch-panel-info-text">
          The expression should return a string matching one of your output port names.
          If no match is found, the default branch (if configured) will be used.
        </span>
      </div>
    </div>
  );
}

// ============================================
// Default Branch Section Component
// ============================================

interface DefaultBranchSectionProps {
  defaultBranch?: string;
  onChange: (defaultBranch: string | undefined) => void;
}

/**
 * Default branch configuration section.
 */
function DefaultBranchSection({ defaultBranch, onChange }: DefaultBranchSectionProps) {
  const [enabled, setEnabled] = useState(!!defaultBranch);
  
  const handleToggle = (newEnabled: boolean) => {
    setEnabled(newEnabled);
    if (newEnabled) {
      onChange('default');
    } else {
      onChange(undefined);
    }
  };
  
  return (
    <div className="switch-default-branch-section">
      <Field label="Enable Default Branch">
        <label className="switch-panel-toggle">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => handleToggle(e.target.checked)}
          />
          <span className="switch-panel-toggle-slider" />
          <span className="switch-panel-toggle-label">
            {enabled ? 'Enabled' : 'Disabled'}
          </span>
        </label>
      </Field>
      
      {enabled && (
        <Field label="Default Port Name" hint="fallback branch">
          <input
            type="text"
            className="switch-panel-input"
            value={defaultBranch || ''}
            onChange={(e) => onChange(e.target.value || undefined)}
            placeholder="default"
          />
          <div className="switch-panel-field-help">
            This branch is used when no conditions match (condition mode) 
            or when the expression returns an unknown branch name.
          </div>
        </Field>
      )}
    </div>
  );
}

export default SwitchPanel;
