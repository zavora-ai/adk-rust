/**
 * WaitNode Component for ADK Studio
 * 
 * ReactFlow node wrapper for Wait action nodes.
 * Displays wait type and duration preview with comprehensive
 * information about the wait configuration.
 * Shows a live countdown timer while the node is executing.
 * 
 * Requirements: 9.1, 12.1, 12.3
 */

import { memo, useState, useEffect, useRef } from 'react';
import { ActionNodeBase } from './ActionNodeBase';
import type { WaitNodeConfig, WaitType, TimeUnit } from '../../types/actionNodes';
import '../../styles/waitNode.css';

interface WaitNodeData extends WaitNodeConfig {
  isActive?: boolean;
}

interface Props {
  data: WaitNodeData;
  selected?: boolean;
}

const WAIT_TYPE_LABELS: Record<WaitType, string> = {
  fixed: 'Fixed',
  until: 'Until',
  webhook: 'Webhook',
  condition: 'Condition',
};

const WAIT_TYPE_ICONS: Record<WaitType, string> = {
  fixed: '⏱️',
  until: '📅',
  webhook: '🔗',
  condition: '🔄',
};

const TIME_UNIT_LABELS: Record<TimeUnit, string> = {
  ms: 'ms',
  s: 'sec',
  m: 'min',
  h: 'hr',
};

/** Convert duration + unit to milliseconds */
function toMs(duration: number, unit: TimeUnit): number {
  switch (unit) {
    case 'ms': return duration;
    case 's': return duration * 1000;
    case 'm': return duration * 60_000;
    case 'h': return duration * 3_600_000;
  }
}

/** Format remaining ms into a human-readable countdown string */
function formatCountdown(ms: number): string {
  if (ms <= 0) return '0s';
  const totalSec = Math.ceil(ms / 1000);
  if (totalSec < 60) return `${totalSec}s`;
  const m = Math.floor(totalSec / 60);
  const s = totalSec % 60;
  if (m < 60) return s > 0 ? `${m}m ${s}s` : `${m}m`;
  const h = Math.floor(m / 60);
  const rm = m % 60;
  return rm > 0 ? `${h}h ${rm}m` : `${h}h`;
}

function formatDuration(duration: number, unit: TimeUnit): string {
  return `${duration}${TIME_UNIT_LABELS[unit]}`;
}

function formatConditionInfo(pollInterval: number, maxWait: number): string {
  const pollSec = Math.round(pollInterval / 1000);
  const maxSec = Math.round(maxWait / 1000);
  return `Poll: ${pollSec}s, Max: ${maxSec}s`;
}

export const WaitNode = memo(function WaitNode({ data, selected }: Props) {
  const waitType = data.waitType || 'fixed';
  const typeLabel = WAIT_TYPE_LABELS[waitType];
  const typeIcon = WAIT_TYPE_ICONS[waitType];

  // Countdown state
  const [remaining, setRemaining] = useState<number | null>(null);
  const startTimeRef = useRef<number | null>(null);
  const totalMsRef = useRef<number>(0);
  const wasActiveRef = useRef(false);
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Only start countdown on the false→true transition of isActive
  useEffect(() => {
    const isActive = !!data.isActive;

    if (isActive && !wasActiveRef.current && waitType === 'fixed' && data.fixed) {
      // Rising edge: just became active — start the countdown once
      const total = toMs(data.fixed.duration, data.fixed.unit);
      totalMsRef.current = total;
      startTimeRef.current = Date.now();
      setRemaining(total);

      intervalRef.current = setInterval(() => {
        const elapsed = Date.now() - (startTimeRef.current ?? Date.now());
        const left = Math.max(0, total - elapsed);
        setRemaining(left);
        if (left <= 0 && intervalRef.current) {
          clearInterval(intervalRef.current);
          intervalRef.current = null;
        }
      }, 100);
    } else if (!isActive && wasActiveRef.current) {
      // Falling edge: no longer active — clean up
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
      setRemaining(null);
      startTimeRef.current = null;
    }

    wasActiveRef.current = isActive;
  });

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (intervalRef.current) clearInterval(intervalRef.current);
    };
  }, []);

  const getPreviewText = (): string | null => {
    switch (waitType) {
      case 'fixed':
        if (data.fixed) return formatDuration(data.fixed.duration, data.fixed.unit);
        return null;
      case 'until':
        if (data.until?.timestamp) {
          const ts = data.until.timestamp;
          return ts.length > 16 ? ts.substring(0, 16) + '...' : ts;
        }
        return null;
      case 'webhook':
        if (data.webhook?.path) {
          const path = data.webhook.path;
          return path.length > 20 ? '...' + path.slice(-17) : path;
        }
        return null;
      case 'condition':
        if (data.condition) {
          return formatConditionInfo(data.condition.pollInterval, data.condition.maxWait);
        }
        return null;
      default:
        return null;
    }
  };

  const previewText = getPreviewText();
  const isCountingDown = !!data.isActive && remaining !== null;
  const progress = (remaining !== null && totalMsRef.current > 0)
    ? 1 - remaining / totalMsRef.current
    : 0;

  return (
    <ActionNodeBase
      type="wait"
      label={data.name || 'Wait'}
      isActive={data.isActive}
      isSelected={selected}
      status={data.isActive ? 'running' : 'idle'}
    >
      <div className="wait-node-content">
        {isCountingDown ? (
          <div className="wait-node-countdown">
            <div className="wait-countdown-bar-track">
              <div
                className="wait-countdown-bar-fill"
                style={{ width: `${Math.min(progress * 100, 100)}%` }}
              />
            </div>
            <div className="wait-countdown-text">
              <span className="wait-countdown-icon">{remaining === 0 ? '✓' : '⏳'}</span>
              <span className="wait-countdown-remaining">{remaining === 0 ? 'Done' : formatCountdown(remaining)}</span>
            </div>
          </div>
        ) : (
          <>
            <div className="wait-node-type">
              <span className="wait-node-type-icon">{typeIcon}</span>
              <span className="wait-node-type-badge">{typeLabel}</span>
            </div>
            {previewText && (
              <div className="wait-node-preview">
                <span className="wait-node-preview-text">{previewText}</span>
              </div>
            )}
          </>
        )}
      </div>
    </ActionNodeBase>
  );
});

export default WaitNode;
