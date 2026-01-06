import { useState, useRef, useCallback, useEffect } from 'react';
import { Renderer } from './adk-ui-renderer/Renderer';
import type { UiResponse, Component, UiEvent } from './adk-ui-renderer/types';
import { uiEventToMessage } from './adk-ui-renderer/types';
import { Send, Loader2, Moon, Sun, Monitor } from 'lucide-react';

// Configuration - adjust these to match your adk-server
// Uses same hostname as the page, but on port 8080
const API_BASE = `http://${window.location.hostname}:8080`;
const APP_NAME = 'ui_demo';
const USER_ID = 'user1';

interface Message {
  role: 'user' | 'agent';
  content: string | UiResponse;
  isStreaming?: boolean;
}

// Recursively search for 'components' array in any object
function findComponents(obj: unknown): Component[] {
  if (!obj || typeof obj !== 'object') return [];

  // Check if this object has a 'components' array
  if ('components' in obj && Array.isArray((obj as Record<string, unknown>).components)) {
    return (obj as { components: Component[] }).components;
  }

  // Recursively search in all values
  for (const value of Object.values(obj as Record<string, unknown>)) {
    const found = findComponents(value);
    if (found.length > 0) return found;
  }

  return [];
}

// Try to parse JSON from text (might be nested in other content)
function tryParseJsonWithComponents(text: string): Component[] {
  // Try to find and parse JSON objects that contain 'components'
  const jsonPattern = /\{[^{}]*"components"\s*:\s*\[[^\]]*\][^{}]*\}/g;
  const matches = text.match(jsonPattern);

  if (matches) {
    for (const match of matches) {
      try {
        const parsed = JSON.parse(match);
        if (parsed.components && Array.isArray(parsed.components)) {
          return parsed.components;
        }
      } catch {
        // Not valid JSON
      }
    }
  }

  // Try parsing the whole text as JSON
  try {
    const parsed = JSON.parse(text);
    return findComponents(parsed);
  } catch {
    // Not valid JSON
  }

  return [];
}

function App() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [theme, setTheme] = useState<'light' | 'dark' | 'system'>('system');
  const abortRef = useRef<AbortController | null>(null);

  // Apply theme to document
  useEffect(() => {
    const root = document.documentElement;
    if (theme === 'system') {
      const isDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
      root.classList.toggle('dark', isDark);
    } else {
      root.classList.toggle('dark', theme === 'dark');
    }
  }, [theme]);

  // Create a session if we don't have one
  const ensureSession = async (): Promise<string> => {
    if (sessionId) return sessionId;

    const response = await fetch(`${API_BASE}/api/apps/${APP_NAME}/users/${USER_ID}/sessions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ state: {} }),
    });

    if (!response.ok) {
      throw new Error('Failed to create session');
    }

    const data = await response.json();
    const newSessionId = data.id || data.session_id || `session-${Date.now()}`;
    setSessionId(newSessionId);
    return newSessionId;
  };

  // Handle UI events (form submissions, button clicks)
  const handleUiAction = useCallback(async (event: UiEvent) => {
    if (isLoading) return;

    // Convert UI event to a message
    const eventMessage = uiEventToMessage(event);

    // Add as user message (showing what was submitted)
    setMessages(prev => [...prev, { role: 'user', content: eventMessage }]);
    setIsLoading(true);

    try {
      const sid = await ensureSession();

      // Add streaming placeholder for agent response
      setMessages(prev => [...prev, { role: 'agent', content: '', isStreaming: true }]);

      abortRef.current = new AbortController();

      const response = await fetch(`${API_BASE}/api/run/${APP_NAME}/${USER_ID}/${sid}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          new_message: eventMessage,
        }),
        signal: abortRef.current.signal,
      });

      if (!response.ok) {
        throw new Error(`Server error: ${response.status}`);
      }

      const reader = response.body?.getReader();
      if (!reader) throw new Error('No response body');

      const decoder = new TextDecoder();
      let buffer = '';
      let fullText = '';
      let uiComponents: Component[] = [];

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          if (line.startsWith('data: ')) {
            const eventData = line.slice(6);
            if (eventData === '[DONE]') continue;

            try {
              const evt = JSON.parse(eventData);

              // Look for components anywhere in the event
              const foundComponents = findComponents(evt);
              if (foundComponents.length > 0) {
                uiComponents = foundComponents;
              }

              // Also check text parts
              if (evt.content?.parts) {
                for (const part of evt.content.parts) {
                  if (part.text) {
                    const parsed = tryParseJsonWithComponents(part.text);
                    if (parsed.length > 0) {
                      uiComponents = parsed;
                    } else {
                      fullText += part.text;
                    }
                  }
                }
              }

              // Update the streaming message
              setMessages(prev => {
                const updated = [...prev];
                const lastAgent = updated.findIndex(m => m.role === 'agent' && m.isStreaming);
                if (lastAgent >= 0) {
                  updated[lastAgent] = {
                    role: 'agent',
                    content: uiComponents.length > 0
                      ? { components: uiComponents }
                      : fullText,
                    isStreaming: true,
                  };
                }
                return updated;
              });
            } catch {
              // Ignore parse errors
            }
          }
        }
      }

      // Finalize the message
      setMessages(prev => {
        const updated = [...prev];
        const lastAgent = updated.findIndex(m => m.role === 'agent' && m.isStreaming);
        if (lastAgent >= 0) {
          updated[lastAgent] = {
            role: 'agent',
            content: uiComponents.length > 0
              ? { components: uiComponents }
              : fullText || 'Form received!',
            isStreaming: false,
          };
        }
        return updated;
      });
    } catch (error) {
      setMessages(prev => {
        const updated = [...prev];
        const lastAgent = updated.findIndex(m => m.role === 'agent' && m.isStreaming);
        if (lastAgent >= 0) {
          updated[lastAgent] = {
            role: 'agent',
            content: {
              components: [{
                type: 'alert',
                title: 'Error',
                description: `${(error as Error).message}`,
                variant: 'error'
              }]
            },
            isStreaming: false,
          };
        }
        return updated;
      });
    } finally {
      setIsLoading(false);
    }
  }, [isLoading, sessionId]);

  const handleSend = async () => {
    if (!input.trim() || isLoading) return;

    const userMessage = input;
    setInput('');
    setMessages(prev => [...prev, { role: 'user', content: userMessage }]);
    setIsLoading(true);

    try {
      const sid = await ensureSession();

      // Add streaming placeholder for agent
      setMessages(prev => [...prev, { role: 'agent', content: '', isStreaming: true }]);

      abortRef.current = new AbortController();

      const response = await fetch(`${API_BASE}/api/run/${APP_NAME}/${USER_ID}/${sid}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          new_message: userMessage,
        }),
        signal: abortRef.current.signal,
      });

      if (!response.ok) {
        throw new Error(`Server error: ${response.status}`);
      }

      const reader = response.body?.getReader();
      if (!reader) throw new Error('No response body');

      const decoder = new TextDecoder();
      let buffer = '';
      let fullText = '';
      let uiComponents: Component[] = [];

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          if (line.startsWith('data: ')) {
            const data = line.slice(6);
            if (data === '[DONE]') continue;

            try {
              const event = JSON.parse(data);
              console.log('SSE Event:', event);

              // Look for components anywhere in the event
              const foundComponents = findComponents(event);
              if (foundComponents.length > 0) {
                console.log('Found UI components:', foundComponents);
                uiComponents = foundComponents;
              }

              // Also check text parts for embedded JSON
              const textParts: string[] = [];

              // Check event.content.parts
              if (event.content?.parts) {
                for (const part of event.content.parts) {
                  if (part.text) {
                    // Check if text contains UI JSON
                    const parsed = tryParseJsonWithComponents(part.text);
                    if (parsed.length > 0) {
                      uiComponents = parsed;
                    } else {
                      textParts.push(part.text);
                    }
                  }
                  // Check for inline UI data
                  if (part.inline_data?.mime_type === 'application/vnd.adk.ui+json') {
                    try {
                      const uiData = JSON.parse(atob(part.inline_data.data));
                      const found = findComponents(uiData);
                      if (found.length > 0) {
                        uiComponents = found;
                      }
                    } catch {
                      // Invalid UI data
                    }
                  }
                }
              }

              // Check event.actions for tool responses (actions is an object, not array)
              if (event.actions && typeof event.actions === 'object') {
                const found = findComponents(event.actions);
                if (found.length > 0) {
                  uiComponents = found;
                }
              }

              // Accumulate text (skip if we found UI components)
              if (textParts.length > 0 && uiComponents.length === 0) {
                fullText += textParts.join('');
              }

              // Update the streaming message
              setMessages(prev => {
                const updated = [...prev];
                const lastAgent = updated.findIndex(m => m.role === 'agent' && m.isStreaming);
                if (lastAgent >= 0) {
                  updated[lastAgent] = {
                    role: 'agent',
                    content: uiComponents.length > 0
                      ? { components: uiComponents }
                      : fullText,
                    isStreaming: true,
                  };
                  // Check for theme in the response
                  if (event.theme && ['light', 'dark', 'system'].includes(event.theme)) {
                    setTheme(event.theme);
                  }
                }
                return updated;
              });
            } catch (e) {
              console.error('Parse error:', e, 'for line:', line);
            }
          }
        }
      }

      // Finalize the message
      setMessages(prev => {
        const updated = [...prev];
        const lastAgent = updated.findIndex(m => m.role === 'agent' && m.isStreaming);
        if (lastAgent >= 0) {
          updated[lastAgent] = {
            role: 'agent',
            content: uiComponents.length > 0
              ? { components: uiComponents }
              : fullText || 'No response',
            isStreaming: false,
          };
        }
        return updated;
      });
    } catch (error) {
      if ((error as Error).name !== 'AbortError') {
        setMessages(prev => {
          const updated = [...prev];
          const lastAgent = updated.findIndex(m => m.role === 'agent' && m.isStreaming);
          if (lastAgent >= 0) {
            updated[lastAgent] = {
              role: 'agent',
              content: {
                components: [{
                  type: 'alert',
                  title: 'Connection Error',
                  description: `Could not connect to server: ${(error as Error).message}. Make sure adk-server is running on ${API_BASE}`,
                  variant: 'error'
                }]
              },
              isStreaming: false,
            };
          }
          return updated;
        });
      }
    } finally {
      setIsLoading(false);
    }
  };

  const isUiResponse = (content: string | UiResponse): content is UiResponse => {
    return typeof content === 'object' && 'components' in content;
  };

  return (
    <div className="min-h-screen bg-gray-50 dark:bg-gray-900 flex flex-col transition-colors duration-300">
      <header className="bg-white dark:bg-gray-800 border-b dark:border-gray-700 p-4 shadow-sm">
        <div className="max-w-3xl mx-auto flex items-center gap-2">
          <div className="w-8 h-8 bg-blue-600 rounded-lg flex items-center justify-center text-white font-bold">A</div>
          <h1 className="font-bold text-xl text-gray-800 dark:text-white">ADK UI Agent</h1>
          <div className="ml-auto flex items-center gap-2">
            <button
              onClick={() => setTheme(theme === 'dark' ? 'light' : theme === 'light' ? 'system' : 'dark')}
              className="p-2 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
              title={`Theme: ${theme}`}
            >
              {theme === 'dark' ? <Moon className="w-4 h-4 text-gray-600 dark:text-gray-300" /> :
                theme === 'light' ? <Sun className="w-4 h-4 text-gray-600" /> :
                  <Monitor className="w-4 h-4 text-gray-600 dark:text-gray-300" />}
            </button>
            {sessionId && (
              <span className="text-xs text-gray-400">Session: {sessionId.slice(0, 8)}...</span>
            )}
          </div>
        </div>
      </header>

      <main className="flex-1 max-w-3xl w-full mx-auto p-4 overflow-y-auto">
        {messages.length === 0 && (
          <div className="mt-8 max-w-2xl mx-auto">
            <h2 className="text-xl font-semibold text-gray-700 dark:text-gray-200 mb-4 text-center">
              🦀 Example Prompts
            </h2>
            <table className="w-full text-sm border-collapse">
              <thead>
                <tr className="bg-gray-100 dark:bg-gray-800">
                  <th className="px-3 py-2 text-left font-semibold text-gray-600 dark:text-gray-300 border dark:border-gray-700">Level</th>
                  <th className="px-3 py-2 text-left font-semibold text-gray-600 dark:text-gray-300 border dark:border-gray-700">Prompt</th>
                  <th className="px-3 py-2 text-left font-semibold text-gray-600 dark:text-gray-300 border dark:border-gray-700">Component</th>
                </tr>
              </thead>
              <tbody className="text-gray-600 dark:text-gray-400">
                {[
                  { level: 'Basic', levelClass: 'text-green-600 dark:text-green-400', prompt: 'I want to register', component: 'Form' },
                  { level: 'Basic', levelClass: 'text-green-600 dark:text-green-400', prompt: 'Show me my profile in dark mode', component: 'Card + Theme' },
                  { level: 'Medium', levelClass: 'text-yellow-600 dark:text-yellow-400', prompt: 'Show a dashboard with light theme', component: 'Layout' },
                  { level: 'Medium', levelClass: 'text-yellow-600 dark:text-yellow-400', prompt: 'List all users with email and role', component: 'Table' },
                  { level: 'Advanced', levelClass: 'text-orange-600 dark:text-orange-400', prompt: 'Create an analytics dashboard with sales chart in dark mode', component: 'Multi-section' },
                  { level: 'Safety', levelClass: 'text-red-600 dark:text-red-400', prompt: 'Delete my account', component: 'Confirm' },
                ].map((row, i) => (
                  <tr
                    key={i}
                    className="hover:bg-blue-50 dark:hover:bg-blue-900/30 cursor-pointer transition-colors"
                    onClick={() => {
                      setInput(row.prompt);
                      setTimeout(() => {
                        const form = document.querySelector('form');
                        if (form) form.dispatchEvent(new Event('submit', { bubbles: true }));
                      }, 50);
                    }}
                  >
                    <td className="px-3 py-2 border dark:border-gray-700">
                      <span className={`${row.levelClass} font-medium`}>{row.level}</span>
                    </td>
                    <td className="px-3 py-2 border dark:border-gray-700 italic text-blue-600 dark:text-blue-400 underline underline-offset-2">
                      "{row.prompt}"
                    </td>
                    <td className="px-3 py-2 border dark:border-gray-700">{row.component}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <p className="text-center text-gray-500 dark:text-gray-400 mt-4 text-xs">
              👆 Click any prompt to try it instantly
            </p>
          </div>
        )}
        <div className="space-y-6">
          {messages.map((msg, idx) => (
            <div key={idx} className={`flex ${msg.role === 'user' ? 'justify-end' : 'justify-start'}`}>
              <div className={`max-w-[85%] ${msg.role === 'user' ? 'bg-blue-600 text-white p-3 rounded-lg' : ''}`}>
                {msg.role === 'user' ? (
                  <p>{msg.content as string}</p>
                ) : isUiResponse(msg.content) ? (
                  <div className="space-y-4">
                    {(msg.content as UiResponse).components.map((comp, i) => (
                      <Renderer key={i} component={comp} onAction={handleUiAction} theme={(msg.content as UiResponse).theme} />
                    ))}
                  </div>
                ) : (
                  <div className="bg-white dark:bg-gray-800 p-4 rounded-lg border dark:border-gray-700 shadow-sm">
                    {msg.isStreaming && !msg.content && (
                      <div className="flex items-center gap-2 text-gray-400">
                        <Loader2 className="w-4 h-4 animate-spin" />
                        <span>Thinking...</span>
                      </div>
                    )}
                    <p className="whitespace-pre-wrap text-gray-800 dark:text-gray-200">{msg.content as string}</p>
                  </div>
                )}
              </div>
            </div>
          ))}
        </div>
      </main>

      <footer className="bg-white dark:bg-gray-800 border-t dark:border-gray-700 p-4">
        <div className="max-w-3xl mx-auto flex gap-2">
          <input
            type="text"
            value={input}
            onChange={e => setInput(e.target.value)}
            onKeyDown={e => e.key === 'Enter' && handleSend()}
            placeholder="Type a message..."
            disabled={isLoading}
            className="flex-1 px-4 py-2 border dark:border-gray-600 rounded-lg focus:ring-2 focus:ring-blue-500 outline-none disabled:bg-gray-100 dark:disabled:bg-gray-700 bg-white dark:bg-gray-700 text-gray-800 dark:text-white placeholder-gray-400"
          />
          <button
            onClick={handleSend}
            disabled={isLoading}
            className="bg-blue-600 text-white p-2 rounded-lg hover:bg-blue-700 transition-colors disabled:opacity-50"
          >
            {isLoading ? <Loader2 className="w-5 h-5 animate-spin" /> : <Send className="w-5 h-5" />}
          </button>
        </div>
      </footer>
    </div>
  );
}

export default App;
