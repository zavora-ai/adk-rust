/**
 * LLM Provider and Model definitions for ADK Studio
 * 
 * Updated: January 2026
 * Sources: Official documentation from Google, OpenAI, Anthropic, DeepSeek, Groq, Ollama
 */

export interface ModelInfo {
  id: string;
  name: string;
  description: string;
  contextWindow: number;
  capabilities: ('text' | 'vision' | 'audio' | 'code' | 'reasoning' | 'tools')[];
  tier: 'free' | 'standard' | 'premium';
}

export interface ProviderInfo {
  id: string;
  name: string;
  icon: string;
  envVar: string;
  envVarAlt?: string;
  docsUrl: string;
  models: ModelInfo[];
}

export const PROVIDERS: ProviderInfo[] = [
  {
    id: 'gemini',
    name: 'Google Gemini',
    icon: '✨',
    envVar: 'GOOGLE_API_KEY',
    envVarAlt: 'GEMINI_API_KEY',
    docsUrl: 'https://ai.google.dev/gemini-api/docs/models',
    models: [
      // Gemini 3 Series (2026)
      {
        id: 'gemini-3-pro',
        name: 'Gemini 3 Pro',
        description: 'Most intelligent model for complex agentic workflows and coding',
        contextWindow: 2000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gemini-3-flash',
        name: 'Gemini 3 Flash',
        description: 'Fast and efficient for most tasks',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'tools'],
        tier: 'standard',
      },
      // Gemini 2.5 Series (2025)
      {
        id: 'gemini-2.5-pro',
        name: 'Gemini 2.5 Pro',
        description: 'Advanced reasoning and multimodal understanding',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gemini-2.5-flash',
        name: 'Gemini 2.5 Flash',
        description: 'Balanced speed and capability',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gemini-2.5-flash-lite',
        name: 'Gemini 2.5 Flash Lite',
        description: 'Ultra-fast for high-volume tasks',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'free',
      },
      // Gemini 2.0 Series (Legacy)
      {
        id: 'gemini-2.0-flash',
        name: 'Gemini 2.0 Flash',
        description: 'Fast multimodal model (retiring March 2026)',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'tools'],
        tier: 'standard',
      },
      // Native Audio
      {
        id: 'gemini-live-2.5-flash-native-audio',
        name: 'Gemini Live 2.5 Flash Native Audio',
        description: 'Real-time voice conversations',
        contextWindow: 32000,
        capabilities: ['text', 'audio', 'tools'],
        tier: 'premium',
      },
    ],
  },
  {
    id: 'openai',
    name: 'OpenAI',
    icon: '🤖',
    envVar: 'OPENAI_API_KEY',
    docsUrl: 'https://platform.openai.com/docs/models',
    models: [
      // GPT-5 Series (2025)
      {
        id: 'gpt-5',
        name: 'GPT-5',
        description: 'State-of-the-art unified model with adaptive thinking',
        contextWindow: 256000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gpt-5-mini',
        name: 'GPT-5 Mini',
        description: 'Efficient version for most tasks',
        contextWindow: 128000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gpt-5.1',
        name: 'GPT-5.1',
        description: 'Latest iteration with improved performance',
        contextWindow: 256000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      // GPT-4 Series (Legacy)
      {
        id: 'gpt-4o',
        name: 'GPT-4o',
        description: 'Multimodal model (deprecated August 2025)',
        contextWindow: 128000,
        capabilities: ['text', 'vision', 'audio', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gpt-4o-mini',
        name: 'GPT-4o Mini',
        description: 'Fast and affordable (deprecated August 2025)',
        contextWindow: 128000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'free',
      },
    ],
  },
  {
    id: 'anthropic',
    name: 'Anthropic Claude',
    icon: '🎭',
    envVar: 'ANTHROPIC_API_KEY',
    docsUrl: 'https://docs.anthropic.com/en/docs/about-claude/models',
    models: [
      // Claude 4.5 Series (November 2025)
      {
        id: 'claude-opus-4.5',
        name: 'Claude Opus 4.5',
        description: 'Most capable model for complex autonomous tasks',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'claude-sonnet-4.5',
        name: 'Claude Sonnet 4.5',
        description: 'Balanced intelligence and cost for production',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'claude-haiku-4.5',
        name: 'Claude Haiku 4.5',
        description: 'Ultra-efficient for high-volume workloads',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'free',
      },
      // Claude 4 Series (May 2025)
      {
        id: 'claude-opus-4',
        name: 'Claude Opus 4',
        description: 'Hybrid model with extended thinking',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'claude-sonnet-4',
        name: 'Claude Sonnet 4',
        description: 'Balanced model with extended thinking',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
    ],
  },
  {
    id: 'deepseek',
    name: 'DeepSeek',
    icon: '🔍',
    envVar: 'DEEPSEEK_API_KEY',
    docsUrl: 'https://platform.deepseek.com/docs',
    models: [
      // DeepSeek R1 Series (Reasoning)
      {
        id: 'deepseek-r1-0528',
        name: 'DeepSeek R1 (0528)',
        description: 'Latest reasoning model with enhanced thinking depth',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'deepseek-r1',
        name: 'DeepSeek R1',
        description: 'Advanced reasoning comparable to o1',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      // DeepSeek V3 Series (General)
      {
        id: 'deepseek-v3.1',
        name: 'DeepSeek V3.1',
        description: 'Latest 671B MoE model for general tasks',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'deepseek-chat',
        name: 'DeepSeek Chat (V3)',
        description: '671B MoE model, excellent for code and Chinese',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      // Vision
      {
        id: 'deepseek-vl2',
        name: 'DeepSeek VL2',
        description: 'Vision-language model',
        contextWindow: 32000,
        capabilities: ['text', 'vision', 'code'],
        tier: 'standard',
      },
    ],
  },
  {
    id: 'groq',
    name: 'Groq',
    icon: '⚡',
    envVar: 'GROQ_API_KEY',
    docsUrl: 'https://console.groq.com/docs',
    models: [
      // Llama 4 Series (via Groq)
      {
        id: 'llama-4-scout',
        name: 'Llama 4 Scout (17Bx16E)',
        description: 'Fast Llama 4 inference via Groq LPU',
        contextWindow: 128000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      // Llama 3.2 Series
      {
        id: 'llama-3.2-90b-text-preview',
        name: 'Llama 3.2 90B',
        description: 'Large text model',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'llama-3.2-11b-text-preview',
        name: 'Llama 3.2 11B',
        description: 'Balanced text model',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      {
        id: 'llama-3.2-3b-preview',
        name: 'Llama 3.2 3B',
        description: 'Fast small model',
        contextWindow: 128000,
        capabilities: ['text', 'code'],
        tier: 'free',
      },
      // Llama 3.1 Series
      {
        id: 'llama-3.1-70b-versatile',
        name: 'Llama 3.1 70B Versatile',
        description: 'Versatile large model',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'llama-3.1-8b-instant',
        name: 'Llama 3.1 8B Instant',
        description: 'Ultra-fast instruction model',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      // Mixtral
      {
        id: 'mixtral-8x7b-32768',
        name: 'Mixtral 8x7B',
        description: 'MoE model with 32K context',
        contextWindow: 32768,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
    ],
  },
  {
    id: 'ollama',
    name: 'Ollama (Local)',
    icon: '🦙',
    envVar: 'OLLAMA_HOST',
    docsUrl: 'https://ollama.ai/library',
    models: [
      // Llama Series
      {
        id: 'llama3.3:70b',
        name: 'Llama 3.3 70B',
        description: 'Latest Llama for local deployment',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'premium',
      },
      {
        id: 'llama3.2:3b',
        name: 'Llama 3.2 3B',
        description: 'Efficient small model',
        contextWindow: 128000,
        capabilities: ['text', 'code'],
        tier: 'free',
      },
      {
        id: 'llama3.1:8b',
        name: 'Llama 3.1 8B',
        description: 'Popular balanced model',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      // DeepSeek
      {
        id: 'deepseek-r1:14b',
        name: 'DeepSeek R1 14B',
        description: 'Distilled reasoning model',
        contextWindow: 64000,
        capabilities: ['text', 'code', 'reasoning'],
        tier: 'standard',
      },
      {
        id: 'deepseek-r1:32b',
        name: 'DeepSeek R1 32B',
        description: 'Larger distilled reasoning model',
        contextWindow: 64000,
        capabilities: ['text', 'code', 'reasoning'],
        tier: 'standard',
      },
      // Qwen
      {
        id: 'qwen3:14b',
        name: 'Qwen 3 14B',
        description: 'Strong multilingual and coding',
        contextWindow: 32000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'qwen2.5:7b',
        name: 'Qwen 2.5 7B',
        description: 'Efficient multilingual model',
        contextWindow: 32000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      // Mistral
      {
        id: 'mistral:7b',
        name: 'Mistral 7B',
        description: 'Fast and capable',
        contextWindow: 32000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      {
        id: 'mistral-nemo:12b',
        name: 'Mistral Nemo 12B',
        description: 'Enhanced Mistral variant',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      // Gemma
      {
        id: 'gemma3:9b',
        name: 'Gemma 3 9B',
        description: 'Google\'s efficient open model',
        contextWindow: 8192,
        capabilities: ['text', 'vision', 'code'],
        tier: 'free',
      },
      // Code-specific
      {
        id: 'devstral:24b',
        name: 'Devstral 24B',
        description: 'Optimized for coding tasks',
        contextWindow: 64000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'codellama:13b',
        name: 'Code Llama 13B',
        description: 'Code-focused Llama variant',
        contextWindow: 16000,
        capabilities: ['text', 'code'],
        tier: 'free',
      },
    ],
  },
];

// Helper functions
export function getProviderById(id: string): ProviderInfo | undefined {
  return PROVIDERS.find(p => p.id === id);
}

export function getModelById(providerId: string, modelId: string): ModelInfo | undefined {
  const provider = getProviderById(providerId);
  return provider?.models.find(m => m.id === modelId);
}

export function detectProviderFromModel(modelId: string): string {
  if (!modelId) return 'gemini';
  
  const lowerModel = modelId.toLowerCase();
  
  if (lowerModel.includes('gemini') || lowerModel.includes('gemma')) return 'gemini';
  if (lowerModel.includes('gpt') || lowerModel.includes('o1') || lowerModel.includes('o3')) return 'openai';
  if (lowerModel.includes('claude')) return 'anthropic';
  if (lowerModel.includes('deepseek')) return 'deepseek';
  if (lowerModel.includes('llama') || lowerModel.includes('mixtral')) {
    // Could be Groq or Ollama - check for Ollama-style tags
    if (lowerModel.includes(':')) return 'ollama';
    return 'groq';
  }
  if (lowerModel.includes('qwen') || lowerModel.includes('mistral') || lowerModel.includes('codellama') || lowerModel.includes('devstral')) {
    return 'ollama';
  }
  
  return 'gemini'; // Default
}

export function getCapabilityIcon(capability: string): string {
  switch (capability) {
    case 'text': return '📝';
    case 'vision': return '👁️';
    case 'audio': return '🎤';
    case 'code': return '💻';
    case 'reasoning': return '🧠';
    case 'tools': return '🔧';
    default: return '•';
  }
}

export function getTierColor(tier: string): string {
  switch (tier) {
    case 'free': return 'var(--accent-success)';
    case 'standard': return 'var(--accent-primary)';
    case 'premium': return 'var(--accent-warning)';
    default: return 'var(--text-muted)';
  }
}

export function formatContextWindow(tokens: number): string {
  if (tokens >= 1000000) return `${(tokens / 1000000).toFixed(1)}M`;
  if (tokens >= 1000) return `${(tokens / 1000).toFixed(0)}K`;
  return tokens.toString();
}
