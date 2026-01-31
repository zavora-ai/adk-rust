import { useState, useRef, useEffect, useCallback } from 'react';
import ReactMarkdown from 'react-markdown';
import { useStore } from '../../store';
import { useSSE, TraceEvent } from '../../hooks/useSSE';
import type { StateSnapshot } from '../../types/execution';
import type { Project } from '../../types/project';
import { ConsoleFilters, EventFilter } from './ConsoleFilters';

interface Message {
  role: 'user' | 'assistant';
  content: string;
  agent?: string;
}

type FlowPhase = 'idle' | 'input' | 'output';
type Tab = 'chat' | 'events';

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
}: Props) {
  const { currentProject } = useStore();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [activeTab, setActiveTab] = useState<Tab>('chat');
  const { send, cancel, isStreaming, streamingText, currentAgent, toolCalls, events, clearEvents, sessionId, newSession, iteration, snapshots, currentSnapshotIndex, scrubTo, stateKeys } = useSSE(currentProject?.id ?? null, binaryPath);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const eventsEndRef = useRef<HTMLDivElement>(null);
  const sendingRef = useRef(false);
  const lastAgentRef = useRef<string | null>(null);
  
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
      onActiveAgent?.(currentAgent);
    }
  }, [currentAgent, onActiveAgent]);

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

  // v2.0: Track run status based on streaming state and events
  useEffect(() => {
    if (isStreaming) {
      setRunStatus('running');
    }
  }, [isStreaming]);

  const sendMessage = () => {
    if (!input.trim() || !currentProject || isStreaming || sendingRef.current) return;
    sendingRef.current = true;
    const userMsg = input.trim();
    setInput('');
    setMessages((m) => [...m, { role: 'user', content: userMsg }]);
    onFlowPhase?.('input');
    lastAgentRef.current = null;
    setRunStatus('running');
    setLastError(null);
    
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

  const handleNewSession = () => {
    setMessages([]);
    newSession();
    setRunStatus('idle');
    setLastError(null);
  };

  const handleCancel = () => {
    cancel();
    onFlowPhase?.('idle');
    setRunStatus('idle');
  };

  // v2.0: Clear history (Requirement 13.6)
  const handleClearHistory = () => {
    setMessages([]);
    clearEvents();
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
            <div key={i} className="text-sm" style={{ color: m.role === 'user' ? 'var(--accent-primary)' : 'var(--text-primary)' }}>
              <span className="font-semibold">{m.role === 'user' ? 'You: ' : `${m.agent || 'Agent'}: `}</span>
              {m.role === 'user' ? (
                <span>{m.content}</span>
              ) : (
                <div className="prose prose-sm max-w-none inline" style={{ color: 'var(--text-primary)' }}>
                  <ReactMarkdown>{m.content}</ReactMarkdown>
                </div>
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

      <div className="p-2 border-t flex gap-2" style={{ borderColor: 'var(--border-default)' }}>
        {(() => {
          const workflowState = validateWorkflow(currentProject, binaryPath, buildStatus);
          const isReady = workflowState === 'ready';
          const placeholder = getPlaceholderText(workflowState, buildStatus);
          const isDisabled = !isReady || isStreaming;
          
          return (
            <>
              <input
                type="text"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.repeat && isReady) {
                    e.preventDefault();
                    sendMessage();
                  }
                }}
                placeholder={placeholder}
                className="flex-1 px-3 py-2 rounded text-sm"
                style={{ 
                  backgroundColor: isDisabled ? 'var(--bg-secondary)' : 'var(--bg-primary)', 
                  border: `1px solid ${!isReady ? 'var(--accent-warning)' : 'var(--border-default)'}`,
                  color: isDisabled ? 'var(--text-muted)' : 'var(--text-primary)',
                  cursor: isDisabled ? 'not-allowed' : 'text',
                }}
                disabled={isDisabled}
              />
              <button
                onClick={sendMessage}
                disabled={isDisabled || !input.trim()}
                className="px-4 py-2 rounded text-sm disabled:opacity-50 disabled:cursor-not-allowed"
                style={{ backgroundColor: 'var(--accent-primary)', color: 'white' }}
                title={!isReady ? placeholder : 'Send message'}
              >
                Send
              </button>
            </>
          );
        })()}
      </div>
    </div>
  );
}
