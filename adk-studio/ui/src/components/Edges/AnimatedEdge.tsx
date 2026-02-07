/**
 * AnimatedEdge Component for ADK Studio v2.0
 * 
 * Displays animated edges during execution flow with dashed line animation.
 * Shows execution direction and highlights active paths.
 * 
 * Requirements: 10.1, 10.2
 */

import { useMemo } from 'react';
import { getSmoothStepPath, type EdgeProps } from '@xyflow/react';

/**
 * Data for animated edges
 */
export interface AnimatedEdgeData {
  /** Whether the edge is currently animated (execution flowing through) */
  animated?: boolean;
  /** Whether this edge is part of the execution path */
  isExecutionPath?: boolean;
  /** Whether the source node is currently active */
  sourceActive?: boolean;
  /** Whether the target node is currently active */
  targetActive?: boolean;
  /** Animation speed multiplier (default: 1) */
  animationSpeed?: number;
}

/**
 * AnimatedEdge displays execution flow with dashed line animation.
 * 
 * Features:
 * - Dashed line animation during execution (Requirement 10.1, 10.2)
 * - Direction-aware animation (flows from source to target)
 * - Visual distinction between active and inactive edges
 * - Smooth transitions between states
 * 
 * @see Requirement 10.1: Edge SHALL display animated flow indicator
 * @see Requirement 10.2: Flow animation SHALL use dashed line animation
 */
export function AnimatedEdge({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  data,
  selected,
}: EdgeProps) {
  const [edgePath] = getSmoothStepPath({
    sourceX,
    sourceY,
    sourcePosition,
    targetX,
    targetY,
    targetPosition,
  });

  const edgeData = data as AnimatedEdgeData | undefined;
  const isAnimated = edgeData?.animated || false;
  const isExecutionPath = edgeData?.isExecutionPath || false;
  const animationSpeed = edgeData?.animationSpeed || 1;

  // Calculate animation duration based on speed
  const animationDuration = useMemo(() => {
    return `${0.5 / animationSpeed}s`;
  }, [animationSpeed]);

  // Determine edge styling based on state
  const { strokeColor, strokeWidth, strokeDasharray } = useMemo(() => {
    if (isAnimated) {
      return {
        strokeColor: '#ef4444', // Red for active execution
        strokeWidth: 3,
        strokeDasharray: '8 4',
      };
    }
    if (isExecutionPath) {
      return {
        strokeColor: '#22c55e', // Green for completed execution path
        strokeWidth: 2.5,
        strokeDasharray: 'none',
      };
    }
    if (selected) {
      return {
        strokeColor: '#3b82f6', // Blue for selected
        strokeWidth: 2.5,
        strokeDasharray: 'none',
      };
    }
    return {
      strokeColor: '#6b7280', // Gray default
      strokeWidth: 2,
      strokeDasharray: 'none',
    };
  }, [isAnimated, isExecutionPath, selected]);

  return (
    <>
      {/* Main edge path */}
      <path
        id={id}
        d={edgePath}
        fill="none"
        stroke={strokeColor}
        strokeWidth={strokeWidth}
        strokeDasharray={strokeDasharray}
        strokeLinecap="round"
        className="react-flow__edge-path"
        style={{
          animation: isAnimated ? `dashFlow ${animationDuration} linear infinite` : 'none',
          transition: 'stroke 0.2s ease, stroke-width 0.2s ease',
        }}
      />
      
      {/* Invisible wider path for easier interaction */}
      <path
        d={edgePath}
        fill="none"
        stroke="transparent"
        strokeWidth={20}
        className="react-flow__edge-interaction"
      />
      
      {/* Glow effect for animated edges */}
      {isAnimated && (
        <path
          d={edgePath}
          fill="none"
          stroke={strokeColor}
          strokeWidth={strokeWidth + 4}
          strokeDasharray={strokeDasharray}
          strokeLinecap="round"
          style={{
            animation: `dashFlow ${animationDuration} linear infinite`,
            opacity: 0.3,
            filter: 'blur(4px)',
          }}
        />
      )}
      
      {/* Animation keyframes */}
      <style>{`
        @keyframes dashFlow {
          0% {
            stroke-dashoffset: 0;
          }
          100% {
            stroke-dashoffset: -12;
          }
        }
      `}</style>
    </>
  );
}

export default AnimatedEdge;
