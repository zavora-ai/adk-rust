import { getSmoothStepPath, type EdgeProps } from '@xyflow/react';

export function AnimatedEdge({ id, sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, data }: EdgeProps) {
  const [edgePath] = getSmoothStepPath({ sourceX, sourceY, sourcePosition, targetX, targetY, targetPosition });
  const isActive = data?.animated || false;

  return (
    <>
      <path
        id={id}
        d={edgePath}
        fill="none"
        stroke={isActive ? '#ef4444' : '#6b7280'}
        strokeWidth={isActive ? 3 : 2}
        strokeDasharray={isActive ? '8 4' : 'none'}
        style={{ animation: isActive ? 'dashFlow 0.5s linear infinite' : 'none' }}
      />
      <style>{`@keyframes dashFlow { to { stroke-dashoffset: -12; } }`}</style>
    </>
  );
}
