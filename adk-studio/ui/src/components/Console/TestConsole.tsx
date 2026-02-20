import { useState, useRef, useEffect, useCallback } from 'react';
import ReactMarkdown from 'react-markdown';
import { useStore } from '../../store';
import { useSSE, TraceEvent, FlowPhase } from '../../hooks/useSSE';
import { useWebhookEvents, WebhookNotification } from '../../hooks/useWebhookEvents';
import type { StateSnapshot, InterruptData } from '../../types/execution';
import type { Project } from '../../types/project';
import { ConsoleFilters, EventFilter } from './ConsoleFilters';
import { DebugPanel } from './DebugPanel';
import type { DebugEntry } from '../../types/debug';
import { DEFAULT_MANUAL_TRIGGER_CONFIG, type TriggerNodeConfig, type TriggerType } from '../../types/actionNodes';

interface Message {
  role: 'user' | 'assistant' | 'interrupt';
  content: string;
  agent?: string;
  /** Interrupt data for HITL messages */
  interruptData?: InterruptData;
}

/**
 * Flow phase for edge animations.
 * @see trigger-input-flow Requirements 2.2, 2.3
 * Note: The actual FlowPhase type is now imported from useSSE hook
 */
type Tab = 'chat' | 'events' | 'debug';

/** Build status for summary line */
export type BuildStatus = 'none' | 'building' | 'success' | 'error';

/** Run status for summary line */
export type RunStatus = 'idle' | 'running' | 'success' | 'error';

/** Workflow validation state */
export type WorkflowState = 
  | 'no_trigger'      // No trigger node
  | 'no_agent'        // No agents in workflow
  | 'not_connected'   // Workflow not connected to END
  | 'not_built'       // Valid but not compiled
  | 'ready';          // Ready to run

/** Get informative placeholder text based on workflow state */
function getPlaceholderText(state: WorkflowState, buildStatus: BuildStatus): string {
  switch (state) {
    case 'no_trigger':
      return '⚠️ Add a trigger node to start your workflow';
    case 'no_agent':
      return '⚠️ Add an agent to your workflow';
    case 'not_connected':
      return '⚠️ Connect your workflow to END';
    case 'not_built':
      if (buildStatus === 'building') {
        return '🔨 Building... please wait';
      }
      if (buildStatus === 'error') {
        return '❌ Build failed - check errors and rebuild';
      }
      return '⚙️ Click Build to compile your workflow';
    case 'ready':
      return 'Type a message...';
  }
}

/**
 * Trigger configuration result with type information.
 */
interface TriggerConfigResult {
  /** The trigger type */
  triggerType: TriggerType;
  /** Input label for the chat input */
  inputLabel: string;
  /** Default prompt/placeholder */
  defaultPrompt: string;
}

/**
 * Get trigger configuration from project.
 * Returns the appropriate config based on the trigger type (manual, webhook, or schedule).
 * @see trigger-input-flow Requirements 1.1, 1.2, 2.1
 */
function getTriggerConfig(project: Project | null): TriggerConfigResult | null {
  if (!project) return null;
  
  const actionNodes = project.actionNodes || {};
  const trigger = Object.values(actionNodes)
    .find(node => node.type === 'trigger');
  
  if (!trigger || trigger.type !== 'trigger') return null;
  
  const triggerType = trigger.triggerType;
  
  switch (triggerType) {
    case 'manual':
      return {
        triggerType: 'manual',
        inputLabel: trigger.manual?.inputLabel || DEFAULT_MANUAL_TRIGGER_CONFIG.inputLabel,
        defaultPrompt: trigger.manual?.defaultPrompt || DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt,
      };
    
    case 'webhook':
      return {
        triggerType: 'webhook',
        inputLabel: 'Webhook Input',
        defaultPrompt: trigger.webhook?.path 
          ? `Webhook payload for ${trigger.webhook.path}` 
          : 'Enter webhook test payload...',
      };
    
    case 'schedule':
      return {
        triggerType: 'schedule',
        inputLabel: 'Schedule Input',
        defaultPrompt: trigger.schedule?.defaultPrompt || 'Scheduled trigger fired',
      };
    
    case 'event':
      return {
        triggerType: 'event',
        inputLabel: 'Event Input',
        defaultPrompt: trigger.event?.eventType 
          ? `Event: ${trigger.event.eventType}` 
          : 'Enter event payload...',
      };
    
    default:
      return {
        triggerType: 'manual',
        inputLabel: DEFAULT_MANUAL_TRIGGER_CONFIG.inputLabel,
        defaultPrompt: DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt,
      };
  }
}

/**
 * Input context for the chat input field.
 * Determines label, placeholder, and disabled state based on workflow state.
 */
interface InputContext {
  /** Label shown above the input field */
  label: string;
  /** Placeholder text inside the input field */
  placeholder: string;
  /** Whether the input is disabled */
  disabled: boolean;
}

/**
 * Get input context based on workflow state, trigger configuration, and interrupt state.
 * @see trigger-input-flow Requirements 2.1, 3.1, 4.1
 */
function getInputContext(
  workflowState: WorkflowState,
  buildStatus: BuildStatus,
  triggerConfig: TriggerConfigResult | null,
  interrupt: InterruptData | null
): InputContext {
  // If there's an active interrupt, show interrupt-specific context
  // @see trigger-input-flow Requirements 3.1, 4.1
  if (interrupt) {
    return {
      label: `⚠️ ${interrupt.nodeId} requires input`,
      placeholder: interrupt.message,
      disabled: false,
    };
  }
  
  // If workflow is not ready, show informative placeholder and disable input
  if (workflowState !== 'ready') {
    return {
      label: '',
      placeholder: getPlaceholderText(workflowState, buildStatus),
      disabled: true,
    };
  }
  
  // Workflow is ready - use trigger configuration
  return {
    label: triggerConfig?.inputLabel || DEFAULT_MANUAL_TRIGGER_CONFIG.inputLabel,
    placeholder: triggerConfig?.defaultPrompt || DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt,
    disabled: false,
  };
}

interface Props {
  onFlowPhase?: (phase: FlowPhase) => void;
  onActiveAgent?: (agent: string | null) => void;
  onIteration?: (iter: number) => void;
  onThought?: (agent: string, thought: string | null) => void;
  binaryPath?: string | null;
  /** v2.0: Callback to pass snapshots and state keys to parent for Timeline and Data Flow Overlays */
  onSnapshotsChange?: (
    snapshots: StateSnapshot[], 
    currentIndex: number, 
    scrubTo: (index: number) => void,
    stateKeys?: Map<string, string[]>
  ) => void;
  /** v2.0: Build status for summary line */
  buildStatus?: BuildStatus;
  /** v2.0: Whether the console is collapsed */
  isCollapsed?: boolean;
  /** v2.0: Callback when collapse state changes */
  onCollapseChange?: (collapsed: boolean) => void;
  /** HITL: Callback when interrupt state changes */
  onInterruptChange?: (interrupt: InterruptData | null) => void;
  /** Auto-send a prompt when this value changes (used by Run button) */
  autoSendPrompt?: string | null;
  /** Callback when autoSendPrompt has been processed */
  onAutoSendComplete?: () => void;
  /** Callback to expose cancel function to parent (for Stop button in toolbar) */
  onCancelReady?: (cancelFn: () => void) => void;
  /** Callback when a trigger notification provides a binary path (schedule/webhook auto-detection) */
  onBinaryPathDetected?: (path: string) => void;
  /** Callback to trigger a build from the console (Send→Build button) */
  onBuild?: () => void;
  /** Debug console: whether debug mode is enabled (controls Debug tab visibility) */
  debugMode?: boolean;
  /** Debug console: debug entries from useSSE hook */
  debugEntries?: DebugEntry[];
  /** Debug console: callback to clear debug entries */
  clearDebugEntries?: () => void;
}

/** Validate workflow and return current state */
function validateWorkflow(project: Project | null, binaryPath: string | null | undefined, buildStatus: BuildStatus): WorkflowState {
  if (!project) return 'no_trigger';
  
  const actionNodes = project.actionNodes || {};
  const agents = project.agents || {};
  const edges = project.workflow?.edges || [];
  
  // Check for trigger node
  const hasTrigger = Object.values(actionNodes).some(node => node.type === 'trigger');
  if (!hasTrigger) return 'no_trigger';
  
  // Check for at least one agent
  const hasAgent = Object.keys(agents).length > 0;
  if (!hasAgent) return 'no_agent';
  
  // Check if workflow is connected to END
  // Find all nodes that can reach END
  const nodesWithOutgoingToEnd = edges.filter(e => e.to === 'END').map(e => e.from);
  const hasEndConnection = nodesWithOutgoingToEnd.length > 0;
  if (!hasEndConnection) return 'not_connected';
  
  // Check if built
  if (!binaryPath || buildStatus === 'none' || buildStatus === 'error') {
    return 'not_built';
  }
  
  return 'ready';
}

export function TestConsole({ 
  onFlowPhase, 
  onActiveAgent, 
  onIteration, 
  onThought, 
  binaryPath, 
  onSnapshotsChange,
  buildStatus = 'none',
  isCollapsed: controlledCollapsed,
  onCollapseChange,
  onInterruptChange,
  autoSendPrompt,
  onAutoSendComplete,
  onCancelReady,
  onBinaryPathDetected,
  onBuild,
  debugMode = false,
  debugEntries: debugEntriesProp,
  clearDebugEntries: clearDebugEntriesProp,
}: Props) {
  const { currentProject, updateActionNode } = useStore();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [activeTab, setActiveTab] = useState<Tab>('chat');
  const { 
    send, 
    cancel, 
    isStreaming, 
    streamingText, 
    currentAgent, 
    toolCalls, 
    events, 
    clearEvents, 
    sessionId, 
    newSession, 
    iteration, 
    snapshots, 
    currentSnapshotIndex, 
    scrubTo, 
    stateKeys,
    // HITL: Interrupt state from useSSE
    // @see trigger-input-flow Requirements 3.1, 3.2
    interrupt,
    setInterrupt,
    setFlowPhase,
    // Edge animation: queue of node_start events
    nodeStartQueueRef,
    nodeStartTick,
    // Debug console: debug entries and clear callback
    debugEntries: hookDebugEntries,
    clearDebugEntries: hookClearDebugEntries,
  } = useSSE(currentProject?.id ?? null, binaryPath);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const eventsEndRef = useRef<HTMLDivElement>(null);
  const sendingRef = useRef(false);
  const lastAgentRef = useRef<string | null>(null);
  
  // Auto-clear session when a build succeeds.
  // This ensures stale conversation history from the old binary doesn't persist.
  // When Set/Transform node values change and the project is rebuilt, the old session
  // process is running the OLD binary — we need to kill it and start fresh.
  const prevBuildStatusRef = useRef<BuildStatus>(buildStatus);
  const hadMessagesRef = useRef(false);
  useEffect(() => { hadMessagesRef.current = messages.length > 0; }, [messages.length]);
  useEffect(() => {
    const prev = prevBuildStatusRef.current;
    prevBuildStatusRef.current = buildStatus;
    // Detect transition to 'success' from 'building' (a fresh build just completed)
    if (prev === 'building' && buildStatus === 'success') {
      console.log('[TestConsole] Build succeeded, starting fresh session');
      // Only show notification if there was an active conversation
      if (hadMessagesRef.current) {
        setMessages([{ role: 'assistant', content: '🔄 Build complete — session refreshed with updated workflow.' }]);
      }
      newSession();
    }
  }, [buildStatus, newSession]);
  
  // Webhook events: Subscribe to webhook notifications and auto-trigger workflow
  const handleWebhookReceived = useCallback((notification: WebhookNotification) => {
    // Determine the effective binary path: use prop or notification's binary_path
    const effectiveBinaryPath = binaryPath || notification.binary_path;
    
    // If we don't have a binary path from either source, or already streaming, skip
    if (!effectiveBinaryPath || isStreaming || sendingRef.current) {
      console.log('[TestConsole] Ignoring webhook - not ready or already streaming');
      return;
    }
    
    // If the notification provided a binary path we didn't have, notify parent
    if (!binaryPath && notification.binary_path) {
      onBinaryPathDetected?.(notification.binary_path);
    }
    
    const isScheduleTrigger = notification.method === 'SCHEDULE';
    const isEventTrigger = notification.method === 'EVENT';
    const payload = notification.payload as Record<string, unknown>;
    
    // Extract input from payload (for schedule triggers, this is the default prompt)
    const inputFromPayload = typeof payload?.input === 'string' ? payload.input : null;
    
    // Add a message showing the trigger was received
    const payloadStr = typeof notification.payload === 'string' 
      ? notification.payload 
      : JSON.stringify(notification.payload, null, 2);
    
    const triggerIcon = isScheduleTrigger ? '⏰' : isEventTrigger ? '⚡' : '🔗';
    const triggerLabel = isScheduleTrigger ? 'Schedule triggered' : isEventTrigger ? 'Event received' : 'Webhook received';
    
    setMessages((m) => [...m, { 
      role: 'user', 
      content: `${triggerIcon} ${triggerLabel}: ${notification.path}\n\`\`\`json\n${payloadStr}\n\`\`\`` 
    }]);
    
    // Trigger the workflow
    sendingRef.current = true;
    
    // Phase 1: Set trigger_input phase to animate trigger→START edge
    onFlowPhase?.('trigger_input');
    lastAgentRef.current = null;
    setRunStatus('running');
    setLastError(null);
    
    // Phase 2: After 500ms, transition to 'input' phase for START→agent animation
    setTimeout(() => {
      onFlowPhase?.('input');
    }, 500);
    
    // For schedule triggers with an input, send the input directly
    // For webhooks and events, use __webhook__ marker so SSE handler retrieves stored payload
    const inputToSend = isScheduleTrigger && inputFromPayload 
      ? inputFromPayload 
      : '__webhook__';
    
    send(
      inputToSend,
      (text) => {
        if (text) {
          setMessages((m) => [...m, { role: 'assistant', content: text, agent: lastAgentRef.current || undefined }]);
        }
        onFlowPhase?.('idle');
        sendingRef.current = false;
        setRunStatus('success');
      },
      (error) => {
        setMessages((m) => [...m, { role: 'assistant', content: `Error: ${error}` }]);
        onFlowPhase?.('idle');
        sendingRef.current = false;
        setRunStatus('error');
        setLastError(error);
      },
      notification.session_id // Pass the session_id from the trigger
    );
  }, [binaryPath, isStreaming, onFlowPhase, send, onBinaryPathDetected]);
  
  // Subscribe to webhook events for this project
  const { isConnected: _webhookConnected } = useWebhookEvents(
    currentProject?.id ?? null,
    {
      onWebhook: handleWebhookReceived,
      enabled: true, // Always subscribe - notifications include binary_path
    }
  );
  
  // v2.0: Collapse state (controlled or uncontrolled)
  const [internalCollapsed, setInternalCollapsed] = useState(false);
  const collapsed = controlledCollapsed !== undefined ? controlledCollapsed : internalCollapsed;
  const setCollapsed = useCallback((value: boolean) => {
    if (onCollapseChange) {
      onCollapseChange(value);
    } else {
      setInternalCollapsed(value);
    }
  }, [onCollapseChange]);
  
  // v2.0: Event filtering
  const [eventFilter, setEventFilter] = useState<EventFilter>('all');
  
  // v2.0: Auto-scroll preference
  const [autoScroll, setAutoScroll] = useState(true);
  
  // v2.0: Run status tracking
  const [runStatus, setRunStatus] = useState<RunStatus>('idle');
  const [lastError, setLastError] = useState<string | null>(null);

  // Debug console: resolve debug entries and clear callback (prefer props, fall back to hook)
  const debugEntries = debugEntriesProp ?? hookDebugEntries;
  const clearDebugEntries = clearDebugEntriesProp ?? hookClearDebugEntries;

  // Debug console: auto-switch tab when debugMode toggles
  // @see debug-console-tab Requirements 1.5, 1.6
  useEffect(() => {
    if (debugMode) {
      setActiveTab('debug');
    } else {
      setActiveTab((prev) => (prev === 'debug' ? 'chat' : prev));
    }
  }, [debugMode]);

  /**
   * Update the manual trigger's default prompt with the latest user input.
   * This allows the trigger to remember the user's last prompt for the Run button.
   */
  const updateTriggerDefaultPrompt = useCallback((prompt: string) => {
    if (!currentProject) return;
    
    const actionNodes = currentProject.actionNodes || {};
    const triggerEntry = Object.entries(actionNodes).find(
      ([, node]) => node.type === 'trigger' && node.triggerType === 'manual'
    );
    
    if (!triggerEntry) return;
    
    const [triggerId, triggerNode] = triggerEntry;
    if (triggerNode.type !== 'trigger') return;
    
    // Update the trigger's manual config with the new default prompt
    const updatedNode: TriggerNodeConfig = {
      ...triggerNode,
      manual: {
        ...(triggerNode.manual || DEFAULT_MANUAL_TRIGGER_CONFIG),
        defaultPrompt: prompt,
      },
    };
    
    updateActionNode(triggerId, updatedNode);
  }, [currentProject, updateActionNode]);

  // v2.0: Pass snapshots and state keys to parent for Timeline and Data Flow Overlays
  useEffect(() => {
    onSnapshotsChange?.(snapshots, currentSnapshotIndex, scrubTo, stateKeys);
  }, [snapshots, currentSnapshotIndex, scrubTo, stateKeys, onSnapshotsChange]);

  useEffect(() => {
    onIteration?.(iteration);
  }, [iteration, onIteration]);

  useEffect(() => {
    if (currentAgent) {
      lastAgentRef.current = currentAgent;
    }
  }, [currentAgent]);

  // Drain the node start queue into onActiveAgent.
  // This ensures every node_start event (including rapid-fire action nodes
  // that complete in <1ms) gets forwarded to the animation queue, even when
  // React batches the currentAgent state updates.
  useEffect(() => {
    const queue = nodeStartQueueRef.current;
    if (queue.length === 0) return;
    // Drain all queued nodes
    const nodes = queue.splice(0, queue.length);
    for (const node of nodes) {
      lastAgentRef.current = node;
      onActiveAgent?.(node);
    }
  }, [nodeStartTick, onActiveAgent, nodeStartQueueRef]);

  // v2.0: Auto-scroll to latest output during execution (Requirement 13.7)
  useEffect(() => {
    if (autoScroll) {
      messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
    }
  }, [messages, streamingText, autoScroll]);

  useEffect(() => {
    if (autoScroll) {
      eventsEndRef.current?.scrollIntoView({ behavior: 'smooth' });
    }
  }, [events, autoScroll]);

  useEffect(() => {
    // Use currentAgent or fallback to lastAgentRef for timing issues
    const agent = currentAgent || lastAgentRef.current;
    if (streamingText && agent) {
      console.log('[TestConsole] Emitting thought:', agent, streamingText.slice(-50));
      onThought?.(agent, streamingText.slice(-150));
    } else if (!isStreaming && lastAgentRef.current) {
      onThought?.(lastAgentRef.current, null);
    }
  }, [streamingText, currentAgent, isStreaming, onThought]);

  useEffect(() => {
    if (streamingText) {
      onFlowPhase?.('output');
    } else if (!isStreaming) {
      onFlowPhase?.('idle');
      onActiveAgent?.(null);
    }
  }, [streamingText, isStreaming, onFlowPhase, onActiveAgent]);

  // When streamingText is cleared during execution (node_start clears it),
  // reset flowPhase to 'input' so edge animations fire for the next node.
  // Without this, the parent's flowPhase stays 'output' from the previous
  // LLM response, suppressing edge animations for subsequent loop iterations.
  const prevStreamingTextRef = useRef('');
  useEffect(() => {
    const wasStreaming = prevStreamingTextRef.current.length > 0;
    prevStreamingTextRef.current = streamingText;
    // Transition: had text → no text, but still streaming = new node started
    if (wasStreaming && !streamingText && isStreaming) {
      onFlowPhase?.('input');
    }
  }, [streamingText, isStreaming, onFlowPhase]);

  // v2.0: Track run status based on streaming state and events
  useEffect(() => {
    if (isStreaming) {
      setRunStatus('running');
    }
  }, [isStreaming]);

  /**
   * HITL: Display interrupt message in chat when interrupt state changes.
   * @see trigger-input-flow Requirements 3.1, 4.2
   */
  useEffect(() => {
    if (interrupt) {
      // Add interrupt message to chat history
      setMessages((m) => [...m, { 
        role: 'interrupt', 
        content: interrupt.message, 
        agent: interrupt.nodeId,
        interruptData: interrupt,
      }]);
      // Notify parent of interrupt state change
      onInterruptChange?.(interrupt);
    }
  }, [interrupt, onInterruptChange]);

  /**
   * HITL: Notify parent when interrupt is cleared (workflow resumed).
   * @see trigger-input-flow Requirements 3.2
   */
  useEffect(() => {
    if (!interrupt) {
      onInterruptChange?.(null);
    }
  }, [interrupt, onInterruptChange]);

  /**
   * HITL: Send resume API call when user responds to an interrupt.
   * @see trigger-input-flow Requirements 3.2
   */
  const sendInterruptResponse = useCallback(async (response: string) => {
    if (!sessionId || !interrupt) return;
    
    try {
      const res = await fetch(`/api/sessions/${sessionId}/resume`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ response }),
      });
      
      if (!res.ok) {
        const errorText = await res.text();
        throw new Error(errorText || 'Failed to resume workflow');
      }
      
      // Add user response to chat history
      setMessages((m) => [...m, { role: 'user', content: response }]);
      
      // Clear interrupt state (will be cleared by SSE resume event, but clear locally too)
      setInterrupt(null);
      setFlowPhase('input');
      
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : 'Failed to resume workflow';
      setMessages((m) => [...m, { role: 'assistant', content: `Error: ${errorMsg}` }]);
      setLastError(errorMsg);
    }
  }, [sessionId, interrupt, setInterrupt, setFlowPhase]);

  const sendMessage = () => {
    if (!input.trim() || !currentProject || sendingRef.current) return;
    
    // HITL: If there's an active interrupt, send response to resume endpoint
    // @see trigger-input-flow Requirements 3.2
    if (interrupt) {
      sendInterruptResponse(input.trim());
      setInput('');
      return;
    }
    
    // Don't allow new messages while streaming (unless responding to interrupt)
    if (isStreaming) return;
    
    sendingRef.current = true;
    const userMsg = input.trim();
    setInput('');
    setMessages((m) => [...m, { role: 'user', content: userMsg }]);
    
    // Update the trigger's default prompt with the latest user input
    // This allows the Run button to use the user's last prompt
    updateTriggerDefaultPrompt(userMsg);
    
    // Phase 1: Set trigger_input phase to animate trigger→START edge
    // @see trigger-input-flow Requirements 2.2, 2.3
    onFlowPhase?.('trigger_input');
    lastAgentRef.current = null;
    setRunStatus('running');
    setLastError(null);
    
    // Phase 2: After 500ms, transition to 'input' phase for START→agent animation
    // This timing allows the trigger→START edge to animate before START→agent
    setTimeout(() => {
      onFlowPhase?.('input');
    }, 500);
    
    send(
      userMsg,
      (text) => {
        if (text) {
          setMessages((m) => [...m, { role: 'assistant', content: text, agent: lastAgentRef.current || undefined }]);
        }
        onFlowPhase?.('idle');
        sendingRef.current = false;
        setRunStatus('success');
      },
      (error) => {
        setMessages((m) => [...m, { role: 'assistant', content: `Error: ${error}` }]);
        onFlowPhase?.('idle');
        sendingRef.current = false;
        setRunStatus('error');
        setLastError(error);
      }
    );
  };

  /**
   * Send a message with a specific prompt (used by Run button).
   * Similar to sendMessage but takes the prompt as a parameter.
   */
  const sendWithPrompt = useCallback((prompt: string) => {
    if (!prompt.trim() || !currentProject || sendingRef.current || isStreaming) return;
    
    sendingRef.current = true;
    const userMsg = prompt.trim();
    setMessages((m) => [...m, { role: 'user', content: userMsg }]);
    
    // Update the trigger's default prompt with the latest user input
    // This allows the Run button to use the user's last prompt
    updateTriggerDefaultPrompt(userMsg);
    
    // Phase 1: Set trigger_input phase to animate trigger→START edge
    onFlowPhase?.('trigger_input');
    lastAgentRef.current = null;
    setRunStatus('running');
    setLastError(null);
    
    // Phase 2: After 500ms, transition to 'input' phase for START→agent animation
    setTimeout(() => {
      onFlowPhase?.('input');
    }, 500);
    
    send(
      userMsg,
      (text) => {
        if (text) {
          setMessages((m) => [...m, { role: 'assistant', content: text, agent: lastAgentRef.current || undefined }]);
        }
        onFlowPhase?.('idle');
        sendingRef.current = false;
        setRunStatus('success');
      },
      (error) => {
        setMessages((m) => [...m, { role: 'assistant', content: `Error: ${error}` }]);
        onFlowPhase?.('idle');
        sendingRef.current = false;
        setRunStatus('error');
        setLastError(error);
      }
    );
  }, [currentProject, isStreaming, onFlowPhase, send, updateTriggerDefaultPrompt]);

  // Handle autoSendPrompt - when Run button is clicked with a default prompt
  useEffect(() => {
    if (autoSendPrompt && !isStreaming && !sendingRef.current) {
      sendWithPrompt(autoSendPrompt);
      onAutoSendComplete?.();
    }
  }, [autoSendPrompt, isStreaming, sendWithPrompt, onAutoSendComplete]);

  const handleNewSession = () => {
    setMessages([]);
    newSession();
    setRunStatus('idle');
    setLastError(null);
  };

  const handleCancel = useCallback(() => {
    cancel();
    onFlowPhase?.('idle');
    setRunStatus('idle');
    sendingRef.current = false; // Reset so Run button works again
  }, [cancel, onFlowPhase]);

  // Expose cancel function to parent (for Stop button in toolbar)
  useEffect(() => {
    onCancelReady?.(handleCancel);
  }, [handleCancel, onCancelReady]);

  // v2.0: Clear history (Requirement 13.6)
  const handleClearHistory = () => {
    setMessages([]);
    clearEvents();
    clearDebugEntries();
    setRunStatus('idle');
    setLastError(null);
  };

  const isThinking = isStreaming && !streamingText;

  const formatTime = (ts: number) => {
    const d = new Date(ts);
    return `${d.toLocaleTimeString('en-US', { hour12: false })}:${String(d.getMilliseconds()).padStart(3, '0')}`;
  };

  const eventIcon = (type: TraceEvent['type']) => {
    switch (type) {
      case 'user': return '👤';
      case 'agent_start': return '▶️';
      case 'agent_end': return '✅';
      case 'model': return '💬';
      case 'tool_call': return '🔧';
      case 'tool_result': return '✓';
      case 'done': return '🏁';
      case 'error': return '❌';
      default: return '•';
    }
  };

  const eventColor = (type: TraceEvent['type']) => {
    switch (type) {
      case 'user': return 'var(--accent-primary)';
      case 'agent_start': return 'var(--accent-success)';
      case 'agent_end': return 'var(--accent-success)';
      case 'model': return 'var(--text-secondary)';
      case 'done': return 'var(--node-sequential)';
      case 'error': return 'var(--accent-error)';
      default: return 'var(--text-muted)';
    }
  };

  // Helper for inline styles
  const getEventColor = (type: TraceEvent['type']) => eventColor(type);

  // v2.0: Filter events based on selected filter (Requirement 13.3)
  const filteredEvents = events.filter(e => {
    switch (eventFilter) {
      case 'model':
        return e.type === 'model' || e.type === 'agent_start' || e.type === 'agent_end';
      case 'tool':
        return e.type === 'tool_call' || e.type === 'tool_result';
      case 'session':
        return e.type === 'user' || e.type === 'done' || e.type === 'error';
      case 'all':
      default:
        return true;
    }
  });

  // v2.0: Build status icon and text for summary line
  const getBuildStatusDisplay = () => {
    switch (buildStatus) {
      case 'building':
        return { icon: '🔨', text: 'Building...', color: 'var(--accent-warning)' };
      case 'success':
        return { icon: '✅', text: 'Built', color: 'var(--accent-success)' };
      case 'error':
        return { icon: '❌', text: 'Build failed', color: 'var(--accent-error)' };
      default:
        return { icon: '⚪', text: 'Not built', color: 'var(--text-muted)' };
    }
  };

  // v2.0: Run status icon and text for summary line
  const getRunStatusDisplay = () => {
    switch (runStatus) {
      case 'running':
        return { icon: '⏳', text: 'Running...', color: 'var(--accent-warning)' };
      case 'success':
        return { icon: '✅', text: 'Success', color: 'var(--accent-success)' };
      case 'error':
        return { icon: '❌', text: 'Error', color: 'var(--accent-error)' };
      default:
        return { icon: '⚪', text: 'Idle', color: 'var(--text-muted)' };
    }
  };

  const buildStatusDisplay = getBuildStatusDisplay();
  const runStatusDisplay = getRunStatusDisplay();

  // v2.0: Collapsed summary view (Requirements 13.1, 13.2, 13.8)
  if (collapsed) {
    return (
      <div 
        className="flex items-center justify-between px-3 py-2 border-t cursor-pointer hover:bg-opacity-50"
        style={{ 
          backgroundColor: 'var(--surface-panel)', 
          borderColor: 'var(--border-default)',
          color: 'var(--text-primary)'
        }}
        onClick={() => setCollapsed(false)}
      >
        <div className="flex items-center gap-4 text-xs">
          <span className="font-medium">Console</span>
          <span style={{ color: buildStatusDisplay.color }}>
            {buildStatusDisplay.icon} {buildStatusDisplay.text}
          </span>
          <span style={{ color: runStatusDisplay.color }}>
            {runStatusDisplay.icon} {runStatusDisplay.text}
          </span>
          {lastError && (
            <span style={{ color: 'var(--accent-error)' }} title={lastError}>
              Last error: {lastError.slice(0, 30)}{lastError.length > 30 ? '...' : ''}
            </span>
          )}
        </div>
        <button 
          className="text-xs px-2 py-1 rounded"
          style={{ color: 'var(--text-secondary)' }}
          onClick={(e) => { e.stopPropagation(); setCollapsed(false); }}
        >
          ▲ Expand
        </button>
      </div>
    );
  }

  return (
    <div 
      className="flex flex-col h-full border-t"
      style={{ 
        backgroundColor: 'var(--surface-panel)', 
        borderColor: 'var(--border-default)',
        color: 'var(--text-primary)'
      }}
    >
      <div 
        className="p-2 border-b text-sm flex justify-between items-center"
        style={{ borderColor: 'var(--border-default)' }}
      >
        <div className="flex gap-1 items-center">
          <button 
            onClick={() => setActiveTab('chat')}
            className="px-3 py-1 rounded text-xs"
            style={{ 
              backgroundColor: activeTab === 'chat' ? 'var(--accent-primary)' : 'transparent',
              color: activeTab === 'chat' ? 'white' : 'var(--text-primary)'
            }}
          >
            💬 Chat
          </button>
          <button 
            onClick={() => setActiveTab('events')}
            className="px-3 py-1 rounded text-xs"
            style={{ 
              backgroundColor: activeTab === 'events' ? 'var(--accent-primary)' : 'transparent',
              color: activeTab === 'events' ? 'white' : 'var(--text-primary)'
            }}
          >
            📋 Events {events.length > 0 && `(${events.length})`}
          </button>
          {/* Debug console: Debug tab button, only visible when debugMode is enabled */}
          {/* @see debug-console-tab Requirements 1.1, 1.2, 1.4 */}
          {debugMode && (
            <button 
              onClick={() => setActiveTab('debug')}
              className="px-3 py-1 rounded text-xs"
              style={{ 
                backgroundColor: activeTab === 'debug' ? 'var(--accent-primary)' : 'transparent',
                color: activeTab === 'debug' ? 'white' : 'var(--text-primary)'
              }}
            >
              🐛 Debug {debugEntries.length > 0 && `(${debugEntries.length})`}
            </button>
          )}
          {sessionId && (
            <span className="ml-2 text-xs" style={{ color: 'var(--text-muted)' }} title={sessionId}>
              Session: {sessionId.slice(0, 8)}...
            </span>
          )}
          {/* v2.0: Summary status in header */}
          <span className="ml-2 text-xs" style={{ color: buildStatusDisplay.color }}>
            {buildStatusDisplay.icon}
          </span>
          <span className="text-xs" style={{ color: runStatusDisplay.color }}>
            {runStatusDisplay.icon}
          </span>
        </div>
        <div className="flex gap-2 items-center">
          {/* v2.0: Clear history button (Requirement 13.6) */}
          <button 
            onClick={handleClearHistory} 
            className="text-xs flex items-center gap-1"
            style={{ color: 'var(--text-muted)' }}
            title="Clear history"
          >
            🗑️ Clear
          </button>
          <button 
            onClick={handleNewSession} 
            className="text-xs flex items-center gap-1"
            style={{ color: 'var(--accent-success)' }}
            title="Start new conversation"
          >
            ➕ New
          </button>
          {isStreaming && (
            <button onClick={handleCancel} className="text-xs" style={{ color: 'var(--accent-error)' }}>Stop</button>
          )}
          {/* v2.0: Collapse button (Requirement 13.1) */}
          <button 
            onClick={() => setCollapsed(true)} 
            className="text-xs px-2 py-1 rounded"
            style={{ color: 'var(--text-secondary)' }}
            title="Collapse console"
          >
            ▼
          </button>
        </div>
      </div>

      {activeTab === 'chat' && (
        <div className="flex-1 overflow-y-auto p-3 space-y-3">
          {messages.length === 0 && !streamingText && !isThinking && (
            <div className="text-sm" style={{ color: 'var(--text-muted)' }}>
              {(() => {
                const workflowState = validateWorkflow(currentProject, binaryPath, buildStatus);
                switch (workflowState) {
                  case 'no_trigger':
                    return (
                      <div className="space-y-2">
                        <p>👋 Welcome! To get started:</p>
                        <ol className="list-decimal list-inside space-y-1 ml-2">
                          <li>Add a <strong>Trigger</strong> node from the palette</li>
                          <li>Add an <strong>Agent</strong> to process requests</li>
                          <li>Click <strong>Build</strong> to compile</li>
                        </ol>
                      </div>
                    );
                  case 'no_agent':
                    return (
                      <div className="space-y-2">
                        <p>✅ Trigger added! Next:</p>
                        <ol className="list-decimal list-inside space-y-1 ml-2">
                          <li>Add an <strong>Agent</strong> from the palette</li>
                          <li>Connect it to your workflow</li>
                          <li>Click <strong>Build</strong> to compile</li>
                        </ol>
                      </div>
                    );
                  case 'not_connected':
                    return (
                      <div className="space-y-2">
                        <p>⚠️ Almost there!</p>
                        <p>Connect your workflow to the <strong>END</strong> node, then click <strong>Build</strong>.</p>
                      </div>
                    );
                  case 'not_built':
                    return (
                      <div className="space-y-2">
                        <p>🎉 Workflow ready!</p>
                        <p>Click <strong>Build</strong> to compile your workflow, then you can start chatting.</p>
                      </div>
                    );
                  case 'ready':
                    return 'Send a message to test your agent...';
                }
              })()}
            </div>
          )}
          {messages.map((m, i) => (
            <div 
              key={i} 
              className={`text-sm ${m.role === 'interrupt' ? 'my-3' : ''}`}
              style={{ 
                color: m.role === 'user' 
                  ? 'var(--accent-primary)' 
                  : m.role === 'interrupt'
                    ? 'var(--accent-warning)'
                    : 'var(--text-primary)' 
              }}
            >
              {/* HITL: Interrupt messages with distinct styling */}
              {/* @see trigger-input-flow Requirements 3.1, 4.2 */}
              {m.role === 'interrupt' ? (
                <div 
                  className="p-3 rounded-lg border-l-4"
                  style={{ 
                    backgroundColor: 'var(--bg-secondary)',
                    borderLeftColor: 'var(--accent-warning)',
                  }}
                >
                  <div className="flex items-center gap-2 mb-2">
                    <span className="text-lg">⚠️</span>
                    <span className="font-semibold">INTERRUPT: {m.agent} requires input</span>
                  </div>
                  <div 
                    className="prose prose-sm max-w-none"
                    style={{ color: 'var(--text-primary)' }}
                  >
                    <ReactMarkdown>{m.content}</ReactMarkdown>
                  </div>
                  {/* Show additional interrupt data if available */}
                  {m.interruptData?.data && Object.keys(m.interruptData.data).length > 0 && (
                    <div 
                      className="mt-2 p-2 rounded text-xs font-mono"
                      style={{ 
                        backgroundColor: 'var(--bg-primary)',
                        color: 'var(--text-secondary)',
                      }}
                    >
                      {Object.entries(m.interruptData.data).map(([key, value]) => (
                        <div key={key} className="mb-1">
                          <span className="font-semibold">{key}:</span>{' '}
                          <span>{typeof value === 'string' ? value : JSON.stringify(value)}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              ) : (
                <>
                  <span className="font-semibold">{m.role === 'user' ? 'You: ' : `${m.agent || 'Agent'}: `}</span>
                  {m.role === 'user' ? (
                    <span>{m.content}</span>
                  ) : (
                    <div className="prose prose-sm max-w-none inline" style={{ color: 'var(--text-primary)' }}>
                      <ReactMarkdown>{m.content}</ReactMarkdown>
                    </div>
                  )}
                </>
              )}
            </div>
          ))}
          {isThinking && (
            <div className="text-sm flex items-center gap-2" style={{ color: 'var(--text-muted)' }}>
              <span className="animate-spin">⏳</span>
              <span>{currentAgent ? `${currentAgent} is thinking...` : 'Thinking...'}</span>
            </div>
          )}
          {streamingText && (
            <div className="text-sm" style={{ color: 'var(--text-primary)' }}>
              <span className="font-semibold">{currentAgent || 'Agent'}: </span>
              <div className="prose prose-sm max-w-none inline" style={{ color: 'var(--text-primary)' }}>
                <ReactMarkdown>{streamingText}</ReactMarkdown>
              </div>
              <span className="animate-pulse">▌</span>
            </div>
          )}
          {toolCalls.length > 0 && isStreaming && (
            <div className="text-xs mt-1" style={{ color: 'var(--accent-warning)' }}>
              Tools used: {toolCalls.map(t => t.name).join(', ')}
            </div>
          )}
          <div ref={messagesEndRef} />
        </div>
      )}

      {activeTab === 'events' && (
        <div className="flex-1 flex flex-col overflow-hidden">
          {/* v2.0: Event filters (Requirement 13.3) */}
          <ConsoleFilters 
            currentFilter={eventFilter} 
            onFilterChange={setEventFilter}
            autoScroll={autoScroll}
            onAutoScrollChange={setAutoScroll}
          />
          <div className="flex-1 overflow-y-auto p-2 font-mono text-xs">
            {filteredEvents.length === 0 && (
              <div style={{ color: 'var(--text-muted)' }}>
                {events.length === 0 
                  ? 'No events yet. Send a message to see the trace.'
                  : 'No events match the current filter.'}
              </div>
            )}
            {filteredEvents.map((e, i) => (
              <div key={i} className="py-1 border-b" style={{ borderColor: 'var(--border-default)' }}>
                <div className="flex gap-2">
                  <span className="w-24 flex-shrink-0" style={{ color: 'var(--text-muted)' }}>{formatTime(e.timestamp)}</span>
                  <span>{eventIcon(e.type)}</span>
                  <span className="flex-1" style={{ color: getEventColor(e.type) }}>
                    {e.agent && <span style={{ color: 'var(--accent-warning)' }} className="mr-2">[{e.agent}]</span>}
                    {e.type === 'user' ? `Input: ${e.data}` : 
                     e.type === 'agent_start' ? `Started ${e.data}` :
                     e.type === 'agent_end' ? `Completed in ${e.data}` :
                     e.type === 'model' ? `Response: ${e.data}` :
                     e.type === 'done' ? `Done (${e.data})` :
                     e.data}
                  </span>
                </div>
                {e.screenshot && (
                  <div className="ml-28 mt-2 mb-2">
                    <img 
                      src={`data:image/png;base64,${e.screenshot}`} 
                      alt="Browser screenshot" 
                      className="max-w-full max-h-64 rounded border"
                      style={{ borderColor: 'var(--border-default)' }}
                    />
                  </div>
                )}
              </div>
            ))}
            <div ref={eventsEndRef} />
          </div>
        </div>
      )}

      {/* Debug console: DebugPanel rendered when debug tab is active */}
      {/* @see debug-console-tab Requirements 1.3 */}
      {activeTab === 'debug' && (
        <div className="flex-1 flex flex-col overflow-hidden">
          <DebugPanel
            debugEntries={debugEntries}
            clearDebugEntries={clearDebugEntries}
            autoScroll={autoScroll}
            onAutoScrollChange={setAutoScroll}
          />
        </div>
      )}

      <div className="p-2 border-t flex flex-col gap-2" style={{ borderColor: 'var(--border-default)' }}>
        {(() => {
          const workflowState = validateWorkflow(currentProject, binaryPath, buildStatus);
          const triggerConfig = getTriggerConfig(currentProject);
          // HITL: Pass interrupt state to getInputContext
          // @see trigger-input-flow Requirements 3.1, 4.1
          const inputContext = getInputContext(workflowState, buildStatus, triggerConfig, interrupt);
          // Allow input when interrupted (to respond) or when workflow is ready
          const isDisabled = inputContext.disabled || (isStreaming && !interrupt);
          
          return (
            <>
              {/* Input label - shown above input when workflow is ready or interrupted (Requirement 2.1, 3.1, 4.1) */}
              {inputContext.label && (
                <label 
                  className="text-xs font-medium px-1"
                  style={{ 
                    // Use warning color for interrupt labels
                    color: interrupt ? 'var(--accent-warning)' : 'var(--accent-primary)',
                    letterSpacing: '0.025em',
                  }}
                >
                  {inputContext.label}
                </label>
              )}
              <div className="flex gap-2">
                <input
                  type="text"
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && !e.repeat && !inputContext.disabled) {
                      e.preventDefault();
                      sendMessage();
                    }
                  }}
                  placeholder={inputContext.placeholder}
                  className="test-console-input flex-1 px-3 py-2 rounded text-sm"
                  style={{ 
                    backgroundColor: isDisabled ? 'var(--bg-secondary)' : 'var(--bg-primary)', 
                    // HITL: Use warning border when interrupted, error border when disabled
                    // @see trigger-input-flow Requirements 3.1, 4.1
                    border: `1px solid ${
                      interrupt 
                        ? 'var(--accent-warning)' 
                        : inputContext.disabled 
                          ? 'var(--accent-warning)' 
                          : 'var(--border-default)'
                    }`,
                    color: isDisabled ? 'var(--text-muted)' : 'var(--text-primary)',
                    cursor: isDisabled ? 'not-allowed' : 'text',
                  }}
                  disabled={isDisabled}
                />
                <button
                  onClick={() => {
                    // If workflow needs building and we have a build callback, trigger build
                    if (workflowState === 'not_built' && onBuild) {
                      onBuild();
                      return;
                    }
                    sendMessage();
                  }}
                  disabled={
                    // Build button: disabled only while actively building
                    workflowState === 'not_built'
                      ? buildStatus === 'building'
                      // Send/Resume button: disabled when input is empty or streaming
                      : isDisabled || !input.trim()
                  }
                  className="px-4 py-2 rounded text-sm disabled:opacity-50 disabled:cursor-not-allowed"
                  style={{ 
                    backgroundColor: workflowState === 'not_built'
                      ? (buildStatus === 'building' ? 'var(--accent-warning)' : 'var(--accent-primary)')
                      : interrupt ? 'var(--accent-warning)' : 'var(--accent-primary)', 
                    color: 'white' 
                  }}
                  title={
                    workflowState === 'not_built'
                      ? (buildStatus === 'building' ? 'Building...' : 'Build your workflow')
                      : inputContext.disabled 
                        ? inputContext.placeholder 
                        : interrupt 
                          ? 'Send response to resume workflow'
                          : 'Send message'
                  }
                >
                  {workflowState === 'not_built'
                    ? (buildStatus === 'building' ? '⏳ Building...' : '🔨 Build')
                    : interrupt ? 'Resume' : 'Send'}
                </button>
              </div>
            </>
          );
        })()}
      </div>
    </div>
  );
}
