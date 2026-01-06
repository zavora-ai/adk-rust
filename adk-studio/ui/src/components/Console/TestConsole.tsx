import { useState, useRef, useEffect } from 'react';
import ReactMarkdown from 'react-markdown';
import { useStore } from '../../store';
import { useSSE, TraceEvent } from '../../hooks/useSSE';

interface Message {
  role: 'user' | 'assistant';
  content: string;
  agent?: string;
}

type FlowPhase = 'idle' | 'input' | 'output';
type Tab = 'chat' | 'events';

interface Props {
  onFlowPhase?: (phase: FlowPhase) => void;
  onActiveAgent?: (agent: string | null) => void;
  onIteration?: (iter: number) => void;
  onThought?: (agent: string, thought: string | null) => void;
  binaryPath?: string | null;
}

export function TestConsole({ onFlowPhase, onActiveAgent, onIteration, onThought, binaryPath }: Props) {
  const { currentProject } = useStore();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [activeTab, setActiveTab] = useState<Tab>('chat');
  const { send, cancel, isStreaming, streamingText, currentAgent, toolCalls, events, sessionId, newSession, iteration } = useSSE(currentProject?.id ?? null, binaryPath);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const eventsEndRef = useRef<HTMLDivElement>(null);
  const sendingRef = useRef(false);
  const lastAgentRef = useRef<string | null>(null);

  useEffect(() => {
    onIteration?.(iteration);
  }, [iteration, onIteration]);

  useEffect(() => {
    if (currentAgent) {
      lastAgentRef.current = currentAgent;
      onActiveAgent?.(currentAgent);
    }
  }, [currentAgent, onActiveAgent]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, streamingText]);

  useEffect(() => {
    eventsEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [events]);

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

  const sendMessage = () => {
    if (!input.trim() || !currentProject || isStreaming || sendingRef.current) return;
    sendingRef.current = true;
    const userMsg = input.trim();
    setInput('');
    setMessages((m) => [...m, { role: 'user', content: userMsg }]);
    onFlowPhase?.('input');
    lastAgentRef.current = null;
    
    send(
      userMsg,
      (text) => {
        if (text) {
          setMessages((m) => [...m, { role: 'assistant', content: text, agent: lastAgentRef.current || undefined }]);
        }
        onFlowPhase?.('idle');
        sendingRef.current = false;
      },
      (error) => {
        setMessages((m) => [...m, { role: 'assistant', content: `Error: ${error}` }]);
        onFlowPhase?.('idle');
        sendingRef.current = false;
      }
    );
  };

  const handleNewSession = () => {
    setMessages([]);
    newSession();
  };

  const handleCancel = () => {
    cancel();
    onFlowPhase?.('idle');
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
      case 'user': return 'text-blue-400';
      case 'agent_start': return 'text-green-400';
      case 'agent_end': return 'text-green-300';
      case 'model': return 'text-gray-300';
      case 'done': return 'text-purple-400';
      case 'error': return 'text-red-400';
      default: return 'text-gray-400';
    }
  };

  return (
    <div className="flex flex-col h-full bg-studio-panel border-t border-gray-700">
      <div className="p-2 border-b border-gray-700 text-sm flex justify-between items-center">
        <div className="flex gap-1 items-center">
          <button 
            onClick={() => setActiveTab('chat')}
            className={`px-3 py-1 rounded text-xs ${activeTab === 'chat' ? 'bg-studio-highlight' : 'hover:bg-gray-700'}`}
          >
            💬 Chat
          </button>
          <button 
            onClick={() => setActiveTab('events')}
            className={`px-3 py-1 rounded text-xs ${activeTab === 'events' ? 'bg-studio-highlight' : 'hover:bg-gray-700'}`}
          >
            📋 Events {events.length > 0 && `(${events.length})`}
          </button>
          {sessionId && (
            <span className="ml-2 text-xs text-gray-500" title={sessionId}>
              Session: {sessionId.slice(0, 8)}...
            </span>
          )}
        </div>
        <div className="flex gap-2">
          <button 
            onClick={handleNewSession} 
            className="text-green-400 text-xs hover:text-green-300 flex items-center gap-1"
            title="Start new conversation"
          >
            ➕ New
          </button>
          {isStreaming && (
            <button onClick={handleCancel} className="text-red-400 text-xs">Stop</button>
          )}
        </div>
      </div>

      {activeTab === 'chat' && (
        <div className="flex-1 overflow-y-auto p-3 space-y-3">
          {messages.length === 0 && !streamingText && !isThinking && (
            <div className="text-gray-500 text-sm">Send a message to test your agent...</div>
          )}
          {messages.map((m, i) => (
            <div key={i} className={`text-sm ${m.role === 'user' ? 'text-blue-400' : 'text-gray-200'}`}>
              <span className="font-semibold">{m.role === 'user' ? 'You: ' : `${m.agent || 'Agent'}: `}</span>
              {m.role === 'user' ? (
                <span>{m.content}</span>
              ) : (
                <div className="prose prose-invert prose-sm max-w-none inline">
                  <ReactMarkdown>{m.content}</ReactMarkdown>
                </div>
              )}
            </div>
          ))}
          {isThinking && (
            <div className="text-sm text-gray-400 flex items-center gap-2">
              <span className="animate-spin">⏳</span>
              <span>{currentAgent ? `${currentAgent} is thinking...` : 'Thinking...'}</span>
            </div>
          )}
          {streamingText && (
            <div className="text-sm text-gray-200">
              <span className="font-semibold">{currentAgent || 'Agent'}: </span>
              <div className="prose prose-invert prose-sm max-w-none inline">
                <ReactMarkdown>{streamingText}</ReactMarkdown>
              </div>
              <span className="animate-pulse">▌</span>
            </div>
          )}
          {toolCalls.length > 0 && isStreaming && (
            <div className="text-xs text-yellow-400 mt-1">
              Tools used: {toolCalls.map(t => t.name).join(', ')}
            </div>
          )}
          <div ref={messagesEndRef} />
        </div>
      )}

      {activeTab === 'events' && (
        <div className="flex-1 overflow-y-auto p-2 font-mono text-xs">
          {events.length === 0 && (
            <div className="text-gray-500">No events yet. Send a message to see the trace.</div>
          )}
          {events.map((e, i) => (
            <div key={i} className="py-1 border-b border-gray-800">
              <div className="flex gap-2">
                <span className="text-gray-500 w-24 flex-shrink-0">{formatTime(e.timestamp)}</span>
                <span>{eventIcon(e.type)}</span>
                <span className={`${eventColor(e.type)} flex-1`}>
                  {e.agent && <span className="text-yellow-400 mr-2">[{e.agent}]</span>}
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
                    className="max-w-full max-h-64 rounded border border-gray-600"
                  />
                </div>
              )}
            </div>
          ))}
          <div ref={eventsEndRef} />
        </div>
      )}

      <div className="p-2 border-t border-gray-700 flex gap-2">
        <input
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.repeat) {
              e.preventDefault();
              sendMessage();
            }
          }}
          placeholder="Type a message..."
          className="flex-1 px-3 py-2 bg-studio-bg border border-gray-600 rounded text-sm"
          disabled={isStreaming}
        />
        <button
          onClick={sendMessage}
          disabled={isStreaming || !input.trim()}
          className="px-4 py-2 bg-studio-highlight rounded text-sm disabled:opacity-50"
        >
          Send
        </button>
      </div>
    </div>
  );
}
