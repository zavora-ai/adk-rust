import { useState } from 'react';
import { useStore } from '../../store';

interface Message {
  role: 'user' | 'assistant';
  content: string;
}

export function TestConsole() {
  const { currentProject } = useStore();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [loading, setLoading] = useState(false);

  const sendMessage = async () => {
    if (!input.trim() || !currentProject || loading) return;

    const userMsg = input.trim();
    setInput('');
    setMessages((m) => [...m, { role: 'user', content: userMsg }]);
    setLoading(true);

    try {
      const res = await fetch(`/api/projects/${currentProject.id}/run`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ input: userMsg }),
      });

      const data = await res.json();
      if (res.ok) {
        setMessages((m) => [...m, { role: 'assistant', content: data.output }]);
      } else {
        setMessages((m) => [...m, { role: 'assistant', content: `Error: ${data.error}` }]);
      }
    } catch (e) {
      setMessages((m) => [...m, { role: 'assistant', content: `Error: ${e}` }]);
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="flex flex-col h-full bg-studio-panel border-t border-gray-700">
      <div className="p-2 border-b border-gray-700 text-sm font-semibold">💬 Test Console</div>
      <div className="flex-1 overflow-y-auto p-3 space-y-3">
        {messages.length === 0 && (
          <div className="text-gray-500 text-sm">Send a message to test your agent...</div>
        )}
        {messages.map((m, i) => (
          <div key={i} className={`text-sm ${m.role === 'user' ? 'text-blue-400' : 'text-gray-200'}`}>
            <span className="font-semibold">{m.role === 'user' ? 'You: ' : 'Agent: '}</span>
            <span className="whitespace-pre-wrap">{m.content}</span>
          </div>
        ))}
        {loading && <div className="text-gray-400 text-sm">Thinking...</div>}
      </div>
      <div className="p-2 border-t border-gray-700 flex gap-2">
        <input
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && sendMessage()}
          placeholder="Type a message..."
          className="flex-1 px-3 py-2 bg-studio-bg border border-gray-600 rounded text-sm"
          disabled={loading}
        />
        <button
          onClick={sendMessage}
          disabled={loading || !input.trim()}
          className="px-4 py-2 bg-studio-highlight rounded text-sm disabled:opacity-50"
        >
          Send
        </button>
      </div>
    </div>
  );
}
