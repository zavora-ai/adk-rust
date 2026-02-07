/**
 * SwitchNode Component for ADK Studio
 * 
 * ReactFlow node wrapper for Switch action nodes.
 * Displays branch count and multiple output handles for conditional routing.
 * 
 * Features:
 * - Visual display of condition branches
 * - Multiple output handles for each branch
 * - Evaluation mode indicator (first_match/all_match)
 * - Expression mode indicator
 * - Default branch support
 * 
 * Requirements: 6.1, 6.2, 6.3, 12.1, 12.3
 */

import { memo } from 'react';
import { ActionNodeBase } from './ActionNodeBase';
import type { SwitchNodeConfig, SwitchCondition, ConditionOperator } from '../../types/actionNodes';

interface SwitchNodeData extends SwitchNodeConfig {
  isActive?: boolean;
}

interface Props {
  data: SwitchNodeData;
  selected?: boolean;
}

/**
 * Operator display labels for condition preview
 */
const OPERATOR_LABELS: Record<ConditionOperator, string> = {
  eq: '=',
  neq: '≠',
  gt: '>',
  lt: '<',
  gte: '≥',
  lte: '≤',
  contains: '∋',
  startsWith: '^',
  endsWith: '$',
  matches: '~',
  in: '∈',
  empty: '∅',
  exists: '∃',
};

/**
 * Get a short preview of a condition for display
 */
function getConditionPreview(condition: SwitchCondition): string {
  const op = OPERATOR_LABELS[condition.operator] || condition.operator;
  const field = condition.field.length > 12 
    ? condition.field.slice(0, 10) + '…' 
    : condition.field;
  
  if (condition.operator === 'empty' || condition.operator === 'exists') {
    return `${field} ${op}`;
  }
  
  const value = condition.value !== undefined 
    ? String(condition.value).slice(0, 8) 
    : '';
  return `${field} ${op} ${value}`;
}

/**
 * SwitchNode displays conditional branching with multiple outputs.
 * 
 * The node shows:
 * - Evaluation mode badge (first_match or all_match)
 * - List of condition branches with previews
 * - Default branch indicator
 * - Expression mode indicator when enabled
 * 
 * Each condition maps to a separate output handle for routing.
 * 
 * @see Requirements 6.1, 6.2, 6.3, 12.1, 12.3
 */
export const SwitchNode = memo(function SwitchNode({ data, selected }: Props) {
  const conditions = data.conditions || [];
  const conditionCount = conditions.length;
  const hasDefault = !!data.defaultBranch;
  const isExpressionMode = data.expressionMode?.enabled;
  
  // Generate output port IDs based on conditions, deduplicating default branch
  // if it matches an existing condition's outputPort
  const conditionPorts = conditions.map((c) => c.outputPort);
  const defaultIsNew = hasDefault && !conditionPorts.includes(data.defaultBranch!);
  const outputPortIds = [
    ...conditionPorts,
    ...(defaultIsNew ? [data.defaultBranch!] : []),
  ];
  const outputPorts = outputPortIds.length;

  return (
    <ActionNodeBase
      type="switch"
      label={data.name || 'Switch'}
      isActive={data.isActive}
      isSelected={selected}
      status={data.isActive ? 'running' : 'idle'}
      outputPorts={Math.max(1, outputPorts)}
      outputPortIds={outputPortIds.length > 0 ? outputPortIds : undefined}
    >
      <div className="switch-node-content">
        {/* Evaluation Mode Badge */}
        <div className="switch-node-header-row">
          <span className={`switch-node-mode-badge ${data.evaluationMode}`}>
            <span className="switch-node-mode-icon">
              {data.evaluationMode === 'first_match' ? '1️⃣' : '🔀'}
            </span>
            <span className="switch-node-mode-label">
              {data.evaluationMode === 'first_match' ? 'First Match' : 'Fan Out'}
            </span>
          </span>
          
          {/* Branch count badge */}
          <span className="switch-node-count-badge">
            {conditionCount} branch{conditionCount !== 1 ? 'es' : ''}
          </span>
        </div>
        
        {/* Expression Mode Indicator */}
        {isExpressionMode && (
          <div className="switch-node-expression-mode">
            <span className="switch-node-expression-icon">📜</span>
            <span className="switch-node-expression-label">Expression Mode</span>
          </div>
        )}
        
        {/* Condition Branches */}
        {!isExpressionMode && conditionCount > 0 && (
          <div className="switch-branches">
            {conditions.slice(0, 3).map((c, i) => (
              <div key={c.id || i} className="branch-indicator">
                <span className="branch-indicator-name">{c.name || `Branch ${i + 1}`}</span>
                <span className="branch-indicator-preview">{getConditionPreview(c)}</span>
                <span className="branch-indicator-port">→ {c.outputPort}</span>
              </div>
            ))}
            {conditionCount > 3 && (
              <div className="branch-indicator branch-indicator-more">
                +{conditionCount - 3} more branch{conditionCount - 3 !== 1 ? 'es' : ''}
              </div>
            )}
          </div>
        )}
        
        {/* Expression Preview */}
        {isExpressionMode && data.expressionMode?.expression && (
          <div className="switch-node-expression-preview">
            <code className="switch-node-expression-code">
              {data.expressionMode.expression.length > 30
                ? data.expressionMode.expression.slice(0, 28) + '…'
                : data.expressionMode.expression}
            </code>
          </div>
        )}
        
        {/* Default Branch */}
        {hasDefault && (
          <div className="branch-indicator branch-indicator-default">
            <span className="branch-indicator-name">Default</span>
            <span className="branch-indicator-port">→ {data.defaultBranch}</span>
          </div>
        )}
        
        {/* Empty state */}
        {conditionCount === 0 && !isExpressionMode && (
          <div className="switch-node-empty">
            No conditions defined
          </div>
        )}
      </div>
    </ActionNodeBase>
  );
});

export default SwitchNode;
