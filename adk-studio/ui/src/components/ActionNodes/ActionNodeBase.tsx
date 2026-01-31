/**
 * ActionNodeBase Component for ADK Studio
 * 
 * Shared wrapper component for all action nodes with:
 * - Colored header bar based on action node type
 * - Node type icon and label
 * - Integrated StatusIndicator
 * - Support for multiple input/output ports
 * - Theme-aware styling
 * - Tooltips for contextual help
 * 
 * Requirements: 12.1, 12.3
 */

import { Handle, Position } from '@xyflow/react';
import { memo, ReactNode } from 'react';
import { StatusIndicator, type NodeStatus } from '../Overlays/StatusIndicator';
import { Tooltip, ACTION_NODE_TOOLTIPS } from '../Overlays/Tooltip';
import type { ActionNodeType } from '../../types/actionNodes';
import { useStore } from '../../store';
import '../../styles/actionNodes.css';

/**
 * Action node color mapping
 * @see Requirements 12.1: Distinct color scheme for action nodes
 */
export const ACTION_NODE_COLORS: Record<ActionNodeType, string> = {
  trigger: '#6366F1',    // Indigo - entry point
  http: '#3B82F6',       // Blue - network
  set: '#8B5CF6',        // Purple - variables
  transform: '#EC4899',  // Pink - data manipulation
  switch: '#F59E0B',     // Amber - branching
  loop: '#10B981',       // Emerald - iteration
  merge: '#06B6D4',      // Cyan - combination
  wait: '#6B7280',       // Gray - timing
  code: '#EF4444',       // Red - custom code
  database: '#14B8A6',   // Teal - storage
  email: '#EA580C',      // Orange - communication
  notification: '#22D3EE', // Sky - notifications
  rss: '#F97316',        // Orange - RSS feeds
  file: '#A855F7',       // Violet - file operations
};

/**
 * Action node icon mapping
 * @see Requirements 12.1: Icon for each action node type
 */
export const ACTION_NODE_ICONS: Record<ActionNodeType, string> = {
  trigger: '🎯',
  http: '🌐',
  set: '📝',
  transform: '⚙️',
  switch: '🔀',
  loop: '🔄',
  merge: '🔗',
  wait: '⏱️',
  code: '💻',
  database: '🗄️',
  email: '📧',
  notification: '🔔',
  rss: '📡',
  file: '📁',
};

/**
 * Action node display labels
 */
export const ACTION_NODE_LABELS: Record<ActionNodeType, string> = {
  trigger: 'Trigger',
  http: 'HTTP',
  set: 'Set',
  transform: 'Transform',
  switch: 'Switch',
  loop: 'Loop',
  merge: 'Merge',
  wait: 'Wait',
  code: 'Code',
  database: 'Database',
  email: 'Email',
  notification: 'Notification',
  rss: 'RSS/Feed',
  file: 'File',
};

interface ActionNodeBaseProps {
  /** Unique node identifier */
  id?: string;
  /** Action node type */
  type: ActionNodeType;
  /** Display label for the node */
  label: string;
  /** Whether the node is currently executing */
  isActive?: boolean;
  /** Whether the node is selected */
  isSelected?: boolean;
  /** Current execution status */
  status?: NodeStatus;
  /** Whether the node has an error */
  hasError?: boolean;
  /** Optional children to render in the node body */
  children?: ReactNode;
  /** Number of input ports (default: 1) */
  inputPorts?: number;
  /** Number of output ports (default: 1) */
  outputPorts?: number;
  /** Custom input port IDs (optional, defaults to input-0, input-1, etc.) */
  inputPortIds?: string[];
  /** Custom output port IDs (optional, defaults to output-0, output-1, etc.) */
  outputPortIds?: string[];
  /** Whether to show tooltip on hover */
  showTooltip?: boolean;
}

/**
 * ActionNodeBase provides a consistent visual structure for all action node types.
 * 
 * Features:
 * - Colored header bar based on action node type (Requirement 12.1)
 * - Action node type icon (Requirement 12.1)
 * - Integrated StatusIndicator
 * - Support for multiple input/output ports (for Switch, Merge nodes)
 * - Selection highlighting with distinct border
 * - Theme-aware styling using CSS variables
 * - Tooltips for contextual help
 * 
 * @see Requirements 12.1, 12.3
 */
export const ActionNodeBase = memo(function ActionNodeBase({
  type,
  label,
  isActive = false,
  isSelected = false,
  status = 'idle',
  hasError = false,
  children,
  inputPorts = 1,
  outputPorts = 1,
  inputPortIds,
  outputPortIds,
  // Tooltips disabled by default on canvas nodes due to ReactFlow transform issues
  // Enable explicitly for sidebar palette items
  showTooltip = false,
}: ActionNodeBaseProps) {
  const color = ACTION_NODE_COLORS[type];
  const icon = ACTION_NODE_ICONS[type];
  const tooltip = ACTION_NODE_TOOLTIPS[type];
  
  // Get layout direction from store
  const layoutDirection = useStore(s => s.layoutDirection);
  const isHorizontal = layoutDirection === 'LR' || layoutDirection === 'RL';
  
  // Build class names for styling
  const containerClasses = [
    'action-node',
    `action-node-${type}`,
    isSelected && 'action-node-selected',
    isActive && 'action-node-active',
    hasError && 'action-node-error',
    status !== 'idle' && `action-node-status-${status}`,
  ].filter(Boolean).join(' ');

  // Generate input handle positions - supports both vertical (top) and horizontal (left) layouts
  const renderInputHandles = () => {
    return Array.from({ length: inputPorts }).map((_, i) => {
      const id = inputPortIds?.[i] ?? `input-${i}`;
      const percent = (i + 1) * 100 / (inputPorts + 1);
      
      // Render handles for both orientations so edges can connect regardless of layout
      return (
        <Handle
          key={id}
          type="target"
          position={isHorizontal ? Position.Left : Position.Top}
          id={id}
          className="action-node-handle action-node-handle-input"
          style={{ 
            ...(isHorizontal 
              ? { top: `${percent}%`, left: undefined }
              : { left: `${percent}%`, top: undefined }
            ),
            background: color,
          }}
        />
      );
    });
  };

  // Generate output handle positions - supports both vertical (bottom) and horizontal (right) layouts
  const renderOutputHandles = () => {
    return Array.from({ length: outputPorts }).map((_, i) => {
      const id = outputPortIds?.[i] ?? `output-${i}`;
      const percent = (i + 1) * 100 / (outputPorts + 1);
      
      return (
        <Handle
          key={id}
          type="source"
          position={isHorizontal ? Position.Right : Position.Bottom}
          id={id}
          className="action-node-handle action-node-handle-output"
          style={{ 
            ...(isHorizontal 
              ? { top: `${percent}%`, right: undefined }
              : { left: `${percent}%`, bottom: undefined }
            ),
            background: color,
          }}
        />
      );
    });
  };

  const nodeContent = (
    <div 
      className={containerClasses}
      style={{ '--action-color': color } as React.CSSProperties}
    >
      {/* Input handles */}
      {renderInputHandles()}
      
      {/* Colored header bar */}
      <div className="action-node-header" style={{ background: color }}>
        <span className="action-node-icon">{icon}</span>
        <span className="action-node-label">{label}</span>
        <span className="action-node-type-badge">{type}</span>
        <StatusIndicator status={isActive ? 'running' : status} size="sm" />
      </div>

      {/* Node body content */}
      {children && (
        <div className="action-node-body">
          {children}
        </div>
      )}

      {/* Error indicator badge */}
      {hasError && (
        <div className="action-node-error-badge">!</div>
      )}

      {/* Output handles */}
      {renderOutputHandles()}
    </div>
  );

  // Wrap with tooltip if enabled
  if (showTooltip && tooltip) {
    return (
      <Tooltip content={tooltip} position="right" delay={500}>
        {nodeContent}
      </Tooltip>
    );
  }

  return nodeContent;
});

export default ActionNodeBase;
