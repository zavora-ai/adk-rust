/**
 * LLM Provider and Model definitions for ADK Studio
 *
 * Updated: February 2026
 * Sources: Official documentation and pricing pages from Google, OpenAI, Anthropic, DeepSeek, Groq, Ollama,
 *          Fireworks, Together, Mistral, Perplexity, Cerebras, SambaNova, AWS Bedrock, Azure AI
 *
 * IMPORTANT: Model IDs must match the exact API identifiers accepted by each provider.
 * - Anthropic uses dated suffixes: claude-sonnet-4-5-20250929
 * - Google uses version-tagged names: gemini-2.5-pro, gemini-3-pro-preview
 * - OpenAI uses simple names: gpt-5, gpt-5-mini
 * - DeepSeek uses mode names: deepseek-chat, deepseek-reasoner
 * - Groq uses full model paths: meta-llama/llama-4-scout-17b-16e-instruct
 * - Ollama uses tag format: llama3.3:70b
 * - Bedrock uses inference profile IDs: us.anthropic.claude-sonnet-4-6
 * - Azure AI uses deployment names or model IDs
 * - Fireworks/Together/Mistral/Perplexity/Cerebras/SambaNova use OpenAI-compatible APIs
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
      // Gemini 3.1 Series (Preview, February 2026)
      {
        id: 'gemini-3.1-pro-preview',
        name: 'Gemini 3.1 Pro Preview',
        description: 'Latest Gemini with improved reasoning, coding, and agentic capabilities',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      // Gemini 3 Series (Preview, January 2026)
      {
        id: 'gemini-3-pro-preview',
        name: 'Gemini 3 Pro Preview',
        description: 'Most intelligent Gemini model for complex reasoning and agentic workflows',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gemini-3-flash-preview',
        name: 'Gemini 3 Flash Preview',
        description: 'Frontier intelligence at Flash speed, great for coding and agents',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      // Gemini 2.5 Series (Stable, 2025)
      {
        id: 'gemini-2.5-pro',
        name: 'Gemini 2.5 Pro',
        description: 'Advanced reasoning and coding with thinking capabilities',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gemini-2.5-flash',
        name: 'Gemini 2.5 Flash',
        description: 'Hybrid reasoning model with thinking budgets',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gemini-2.5-flash-lite',
        name: 'Gemini 2.5 Flash Lite',
        description: 'Low-cost Gemini model for high-volume tasks',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      // Gemini 2.0 Series (Legacy)
      {
        id: 'gemini-2.0-flash',
        name: 'Gemini 2.0 Flash',
        description: 'Fast multimodal model with native tool use',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'audio', 'code', 'tools'],
        tier: 'free',
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
      // GPT-5.2 Series (February 2026)
      {
        id: 'gpt-5.2',
        name: 'GPT-5.2',
        description: 'Newest flagship model with advanced reasoning and coding',
        contextWindow: 400000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gpt-5.2-pro',
        name: 'GPT-5.2 Pro',
        description: 'Pro variant of GPT-5.2 for maximum quality',
        contextWindow: 400000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      // GPT-5.1 Series (Late 2025)
      {
        id: 'gpt-5.1',
        name: 'GPT-5.1',
        description: 'High-capability model with 400K context',
        contextWindow: 400000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      // GPT-5 Series (August 2025)
      {
        id: 'gpt-5',
        name: 'GPT-5',
        description: 'Strongest coding and agentic model with adaptive reasoning',
        contextWindow: 400000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gpt-5-pro',
        name: 'GPT-5 Pro',
        description: 'Pro variant of GPT-5 for complex tasks',
        contextWindow: 400000,
        capabilities: ['text', 'vision', 'audio', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'gpt-5-mini',
        name: 'GPT-5 Mini',
        description: 'Efficient GPT-5 variant for most tasks',
        contextWindow: 400000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gpt-5-nano',
        name: 'GPT-5 Nano',
        description: 'Lowest cost, highest throughput GPT-5 variant',
        contextWindow: 400000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'free',
      },
      // o-Series Reasoning Models
      {
        id: 'o4-mini',
        name: 'o4-mini',
        description: 'Efficient reasoning model with 200K context',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'o3',
        name: 'o3',
        description: 'Advanced reasoning model for complex problem solving',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'o3-pro',
        name: 'o3 Pro',
        description: 'Premium reasoning model for maximum accuracy',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'o3-mini',
        name: 'o3-mini',
        description: 'Smaller reasoning model, fast and cost-effective',
        contextWindow: 200000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'o1',
        name: 'o1',
        description: 'Original reasoning model with deep thinking',
        contextWindow: 128000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      // GPT-4.1 Series (2025)
      {
        id: 'gpt-4.1',
        name: 'GPT-4.1',
        description: 'General purpose model with 1M context window',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gpt-4.1-mini',
        name: 'GPT-4.1 Mini',
        description: 'Efficient variant with 1M context',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gpt-4.1-nano',
        name: 'GPT-4.1 Nano',
        description: 'Cheapest GPT-4.1 variant with 1M context',
        contextWindow: 1000000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      // Codex
      {
        id: 'codex-mini-latest',
        name: 'Codex Mini',
        description: 'Code-specific model optimized for programming tasks',
        contextWindow: 200000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      // GPT-4 Series (Legacy)
      {
        id: 'gpt-4o',
        name: 'GPT-4o',
        description: 'Multimodal model (legacy)',
        contextWindow: 128000,
        capabilities: ['text', 'vision', 'audio', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'gpt-4o-mini',
        name: 'GPT-4o Mini',
        description: 'Fast and affordable (legacy)',
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
      // Claude 4.6 Series (February 2026)
      {
        id: 'claude-opus-4-6',
        name: 'Claude Opus 4.6',
        description: 'Most capable Anthropic model with 1M context (beta)',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'claude-sonnet-4-6',
        name: 'Claude Sonnet 4.6',
        description: 'Best balance of speed and intelligence, strong coding and agentic performance',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      // Claude 4.5 Series (Late 2025)
      {
        id: 'claude-opus-4-5-20251101',
        name: 'Claude Opus 4.5',
        description: 'Most capable model for complex autonomous tasks and coding',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'claude-sonnet-4-5-20250929',
        name: 'Claude Sonnet 4.5',
        description: 'Best balance of intelligence, speed, and cost for production',
        contextWindow: 1000000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'claude-haiku-4-5-20251001',
        name: 'Claude Haiku 4.5',
        description: 'Near-frontier performance at ultra-efficient pricing',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'free',
      },
      // Claude 4 Series (May 2025)
      {
        id: 'claude-opus-4-20250514',
        name: 'Claude Opus 4',
        description: 'Hybrid model with extended thinking',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'claude-sonnet-4-20250514',
        name: 'Claude Sonnet 4',
        description: 'Balanced model with extended thinking',
        contextWindow: 1000000,
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
    docsUrl: 'https://api-docs.deepseek.com',
    models: [
      // DeepSeek V3.2 (2025) — two modes of the same model
      {
        id: 'deepseek-reasoner',
        name: 'DeepSeek Reasoner (V3.2 Thinking)',
        description: 'Thinking mode with chain-of-thought reasoning for complex tasks',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'deepseek-chat',
        name: 'DeepSeek Chat (V3.2)',
        description: 'Non-thinking mode for fast general-purpose tasks',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
    ],
  },
  {
    id: 'groq',
    name: 'Groq',
    icon: '⚡',
    envVar: 'GROQ_API_KEY',
    docsUrl: 'https://console.groq.com/docs/models',
    models: [
      // Llama 4 Series (Preview, via Groq)
      {
        id: 'meta-llama/llama-4-maverick-17b-128e-instruct',
        name: 'Llama 4 Maverick (17Bx128E)',
        description: 'Largest Llama 4 model for multilingual and multimodal tasks',
        contextWindow: 131072,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'meta-llama/llama-4-scout-17b-16e-instruct',
        name: 'Llama 4 Scout (17Bx16E)',
        description: 'Fast general-purpose Llama 4 via Groq LPU',
        contextWindow: 131072,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      // Llama 3.x Series (Production)
      {
        id: 'llama-3.3-70b-versatile',
        name: 'Llama 3.3 70B Versatile',
        description: 'Versatile large model for diverse tasks',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'llama-3.1-8b-instant',
        name: 'Llama 3.1 8B Instant',
        description: 'Ultra-fast instruction model at 560 T/s',
        contextWindow: 131072,
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
  {
    id: 'fireworks',
    name: 'Fireworks AI',
    icon: '🎆',
    envVar: 'FIREWORKS_API_KEY',
    docsUrl: 'https://docs.fireworks.ai/getting-started/introduction',
    models: [
      {
        id: 'accounts/fireworks/models/llama-v3p3-70b-instruct',
        name: 'Llama 3.3 70B Instruct',
        description: 'High-quality open model via Fireworks inference',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'accounts/fireworks/models/llama-v3p1-8b-instruct',
        name: 'Llama 3.1 8B Instruct',
        description: 'Fast and affordable via Fireworks',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      {
        id: 'accounts/fireworks/models/qwen2p5-72b-instruct',
        name: 'Qwen 2.5 72B Instruct',
        description: 'Large multilingual model via Fireworks',
        contextWindow: 32768,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
    ],
  },
  {
    id: 'together',
    name: 'Together AI',
    icon: '🤝',
    envVar: 'TOGETHER_API_KEY',
    docsUrl: 'https://docs.together.ai/docs/introduction',
    models: [
      {
        id: 'meta-llama/Llama-3.3-70B-Instruct-Turbo',
        name: 'Llama 3.3 70B Turbo',
        description: 'Optimized Llama 3.3 via Together inference',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo',
        name: 'Llama 3.1 8B Turbo',
        description: 'Fast and cost-effective via Together',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
      {
        id: 'Qwen/Qwen2.5-72B-Instruct-Turbo',
        name: 'Qwen 2.5 72B Turbo',
        description: 'Large multilingual model via Together',
        contextWindow: 32768,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
    ],
  },
  {
    id: 'mistral',
    name: 'Mistral AI',
    icon: '🌬️',
    envVar: 'MISTRAL_API_KEY',
    docsUrl: 'https://docs.mistral.ai',
    models: [
      {
        id: 'mistral-large-latest',
        name: 'Mistral Large',
        description: 'Flagship model for complex reasoning and coding',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'mistral-small-latest',
        name: 'Mistral Small',
        description: 'Efficient model for everyday tasks',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'codestral-latest',
        name: 'Codestral',
        description: 'Specialized for code generation and completion',
        contextWindow: 32000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
    ],
  },
  {
    id: 'perplexity',
    name: 'Perplexity',
    icon: '🔮',
    envVar: 'PERPLEXITY_API_KEY',
    docsUrl: 'https://docs.perplexity.ai',
    models: [
      {
        id: 'sonar-pro',
        name: 'Sonar Pro',
        description: 'Advanced search-augmented model with citations',
        contextWindow: 200000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'sonar',
        name: 'Sonar',
        description: 'Fast search-augmented model',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'sonar-reasoning-pro',
        name: 'Sonar Reasoning Pro',
        description: 'Deep reasoning with real-time search',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'reasoning'],
        tier: 'premium',
      },
    ],
  },
  {
    id: 'cerebras',
    name: 'Cerebras',
    icon: '🧪',
    envVar: 'CEREBRAS_API_KEY',
    docsUrl: 'https://inference-docs.cerebras.ai',
    models: [
      {
        id: 'llama-3.3-70b',
        name: 'Llama 3.3 70B',
        description: 'Ultra-fast inference on Cerebras wafer-scale hardware',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'llama-3.1-8b',
        name: 'Llama 3.1 8B',
        description: 'Fastest small model inference available',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
    ],
  },
  {
    id: 'sambanova',
    name: 'SambaNova',
    icon: '⚙️',
    envVar: 'SAMBANOVA_API_KEY',
    docsUrl: 'https://community.sambanova.ai/docs',
    models: [
      {
        id: 'Meta-Llama-3.3-70B-Instruct',
        name: 'Llama 3.3 70B Instruct',
        description: 'High-throughput inference on SambaNova RDU',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'Meta-Llama-3.1-8B-Instruct',
        name: 'Llama 3.1 8B Instruct',
        description: 'Fast and efficient on SambaNova hardware',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'free',
      },
    ],
  },
  {
    id: 'bedrock',
    name: 'Amazon Bedrock',
    icon: '🪨',
    envVar: 'AWS_ACCESS_KEY_ID',
    envVarAlt: 'AWS_DEFAULT_REGION',
    docsUrl: 'https://docs.aws.amazon.com/bedrock/latest/userguide/',
    models: [
      {
        id: 'us.anthropic.claude-sonnet-4-6',
        name: 'Claude Sonnet 4.6 (Bedrock)',
        description: 'Claude Sonnet 4.6 via Amazon Bedrock',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'us.anthropic.claude-sonnet-4-20250514-v1:0',
        name: 'Claude Sonnet 4 (Bedrock)',
        description: 'Claude Sonnet 4 via Amazon Bedrock',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'standard',
      },
      {
        id: 'us.anthropic.claude-haiku-4-5-20251001-v1:0',
        name: 'Claude Haiku 4.5 (Bedrock)',
        description: 'Fast and affordable Claude via Bedrock',
        contextWindow: 200000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'free',
      },
      {
        id: 'us.amazon.nova-pro-v1:0',
        name: 'Amazon Nova Pro',
        description: 'Amazon\'s multimodal model for complex tasks',
        contextWindow: 300000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'us.amazon.nova-lite-v1:0',
        name: 'Amazon Nova Lite',
        description: 'Cost-effective multimodal model',
        contextWindow: 300000,
        capabilities: ['text', 'vision', 'code', 'tools'],
        tier: 'free',
      },
    ],
  },
  {
    id: 'azure-ai',
    name: 'Azure AI',
    icon: '☁️',
    envVar: 'AZURE_AI_ENDPOINT',
    envVarAlt: 'AZURE_AI_API_KEY',
    docsUrl: 'https://learn.microsoft.com/en-us/azure/ai-studio/',
    models: [
      {
        id: 'Mistral-large-2411',
        name: 'Mistral Large (Azure)',
        description: 'Mistral Large via Azure AI Inference',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'Meta-Llama-3.3-70B-Instruct',
        name: 'Llama 3.3 70B (Azure)',
        description: 'Llama 3.3 via Azure AI serverless',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
      {
        id: 'Cohere-command-r-plus-08-2024',
        name: 'Cohere Command R+ (Azure)',
        description: 'Cohere\'s RAG-optimized model via Azure',
        contextWindow: 128000,
        capabilities: ['text', 'code', 'tools'],
        tier: 'standard',
      },
    ],
  },
  {
    id: 'xai',
    name: 'xAI',
    icon: '𝕏',
    envVar: 'XAI_API_KEY',
    docsUrl: 'https://docs.x.ai',
    models: [
      {
        id: 'grok-3',
        name: 'Grok 3',
        description: 'Flagship reasoning model from xAI',
        contextWindow: 131072,
        capabilities: ['text', 'vision', 'code', 'reasoning', 'tools'],
        tier: 'premium',
      },
      {
        id: 'grok-3-mini',
        name: 'Grok 3 Mini',
        description: 'Efficient reasoning model from xAI',
        contextWindow: 131072,
        capabilities: ['text', 'code', 'reasoning', 'tools'],
        tier: 'standard',
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
  if (lowerModel.includes('gpt') || lowerModel.includes('o1') || lowerModel.includes('o3') || lowerModel.includes('o4') || lowerModel.includes('codex')) return 'openai';
  if (lowerModel.includes('claude')) return 'anthropic';
  if (lowerModel.startsWith('grok')) return 'xai';
  // Bedrock uses inference profile IDs like "us.anthropic.claude-*" or "us.amazon.nova-*"
  if (lowerModel.startsWith('us.') || lowerModel.startsWith('eu.') || lowerModel.startsWith('ap.')) return 'bedrock';
  if (lowerModel.includes('deepseek') && !lowerModel.includes(':')) return 'deepseek';
  // Perplexity sonar models
  if (lowerModel.includes('sonar')) return 'perplexity';
  // Mistral API models (not Ollama tags)
  if ((lowerModel.includes('mistral-large') || lowerModel.includes('mistral-small') || lowerModel.includes('codestral')) && !lowerModel.includes(':')) return 'mistral';
  // Fireworks uses "accounts/fireworks/models/" prefix
  if (lowerModel.includes('accounts/fireworks/')) return 'fireworks';
  // Together uses org/model format with Turbo suffix
  if (lowerModel.includes('-turbo') && lowerModel.includes('/')) return 'together';
  // Azure AI models (deployment names)
  if (lowerModel.includes('cohere-command') || (lowerModel.includes('mistral') && lowerModel.includes('2024'))) return 'azure-ai';
  if (lowerModel.includes('llama') || lowerModel.includes('mixtral')) {
    // Could be Groq or Ollama - check for Ollama-style tags
    if (lowerModel.includes(':')) return 'ollama';
    // SambaNova uses Meta-Llama prefix
    if (modelId.startsWith('Meta-Llama')) return 'sambanova';
    // Cerebras uses plain llama-X.X-Xb format
    if (/^llama-\d/.test(lowerModel)) return 'cerebras';
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
