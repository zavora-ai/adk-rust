import { Position } from '@xyflow/react';
import { useStore } from '../store';

export function useHandlePositions() {
  const layoutDir = useStore(s => s.layoutDirection);
  const isHorizontal = layoutDir === 'LR' || layoutDir === 'RL';
  return {
    target: isHorizontal ? Position.Left : Position.Top,
    source: isHorizontal ? Position.Right : Position.Bottom,
  };
}
