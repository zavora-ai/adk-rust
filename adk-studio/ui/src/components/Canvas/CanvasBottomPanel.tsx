import type { StateSnapshot } from '../../types/execution';
import type { BuildStatus } from '../Console/TestConsole';
import { TestConsole } from '../Console/TestConsole';
import { TimelineView } from '../Timeline';

/**
 * Flow phase for edge animations.
 * Matches the FlowPhase type defined in Canvas.tsx.
 */
type FlowPhase = 'idle' | 'trigger_input' | 'input' | 'output' | 'interrupted';

export interface CanvasBottomPanelProps {
  // Visibility flags
  showConsole: boolean;
  showTimeline: boolean;
  debugMode: boolean;
  hasRunnableWorkflow: boolean;
  builtBinaryPath: string | null;

  // Timeline state
  snapshots: StateSnapshot[];
  currentSnapshotIndex: number;
  scrubToFn: ((index: number) => void) | null;
  timelineCollapsed: boolean;
  onToggleTimelineCollapse: () => void;

  // Console state
  consoleCollapsed: boolean;
  onConsoleCollapseChange: (collapsed: boolean) => void;
  buildStatus: BuildStatus;
  autoSendPrompt: string | null;
  onAutoSendComplete: () => void;
  onCancelReady: (fn: () => void) => void;

  // Console execution callbacks
  onFlowPhase: (phase: FlowPhase) => void;
  onActiveAgent: (agent: string | null) => void;
  onIteration: (iter: number) => void;
  onThought: (agent: string, thought: string | null) => void;
  onSnapshotsChange: (
    snapshots: StateSnapshot[],
    currentIndex: number,
    scrubTo: (index: number) => void,
    stateKeys?: Map<string, string[]>
  ) => void;
  onInterruptChange: (interrupt: import('../../types/execution').InterruptData | null) => void;

  // Build action (for "Build Required" placeholder)
  onBuild: () => void;
  
  // Binary path detection from trigger notifications
  onBinaryPathDetected?: (path: string) => void;
}

/**
 * CanvasBottomPanel renders the bottom area of the canvas:
 * - Timeline view (execution history, debug mode only)
 * - Console (TestConsole for running workflows)
 * - "Build required" placeholder when project needs building
 * - "Drag to start" placeholder when no agents/action nodes exist
 *
 * @see Requirements 2.4: Canvas delegates console/timeline rendering
 */
export function CanvasBottomPanel({
  showConsole,
  showTimeline,
  debugMode,
  hasRunnableWorkflow,
  builtBinaryPath,
  snapshots,
  currentSnapshotIndex,
  scrubToFn,
  timelineCollapsed,
  onToggleTimelineCollapse,
  consoleCollapsed,
  onConsoleCollapseChange,
  buildStatus,
  autoSendPrompt,
  onAutoSendComplete,
  onCancelReady,
  onFlowPhase,
  onActiveAgent,
  onIteration,
  onThought,
  onSnapshotsChange,
  onInterruptChange,
  onBuild,
  onBinaryPathDetected,
}: CanvasBottomPanelProps) {
  return (
    <>
      {/* Timeline View - shows execution history */}
      {/* Only visible when debug mode is enabled */}
      {showConsole && debugMode && showTimeline && hasRunnableWorkflow && builtBinaryPath && snapshots.length > 0 && (
        <TimelineView
          snapshots={snapshots}
          currentIndex={currentSnapshotIndex}
          onScrub={scrubToFn || (() => {})}
          isCollapsed={timelineCollapsed}
          onToggleCollapse={onToggleTimelineCollapse}
        />
      )}

      {/* Console Area - renders when project has a runnable workflow */}
      {/* Shows even without builtBinaryPath so it can receive schedule/webhook notifications */}
      {showConsole && hasRunnableWorkflow && (
        <div className={consoleCollapsed ? '' : 'h-64'}>
          <TestConsole
            onFlowPhase={onFlowPhase}
            onActiveAgent={onActiveAgent}
            onIteration={onIteration}
            onThought={onThought}
            binaryPath={builtBinaryPath}
            onSnapshotsChange={onSnapshotsChange}
            buildStatus={buildStatus}
            isCollapsed={consoleCollapsed}
            onCollapseChange={onConsoleCollapseChange}
            onInterruptChange={onInterruptChange}
            autoSendPrompt={autoSendPrompt}
            onAutoSendComplete={onAutoSendComplete}
            onCancelReady={onCancelReady}
            onBinaryPathDetected={onBinaryPathDetected}
            onBuild={onBuild}
            debugMode={debugMode}
          />
        </div>
      )}
      {showConsole && !hasRunnableWorkflow && (
        <div
          className="h-32 border-t flex items-center justify-center"
          style={{ backgroundColor: 'var(--surface-panel)', borderColor: 'var(--border-default)', color: 'var(--text-muted)' }}
        >
          Drag an Agent or Action node onto the canvas to get started
        </div>
      )}
    </>
  );
}
