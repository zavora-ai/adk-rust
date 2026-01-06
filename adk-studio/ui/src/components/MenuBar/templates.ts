import type { AgentSchema } from '../../types/project';

export interface Template {
  id: string;
  name: string;
  icon: string;
  description: string;
  agents: Record<string, AgentSchema>;
  edges: Array<{ from: string; to: string }>;
}

export const TEMPLATES: Template[] = [
  {
    id: 'simple_chat',
    name: 'Simple Chat Agent',
    icon: '💬',
    description: 'A basic conversational agent',
    agents: {
      'chat_agent': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a helpful, friendly assistant. Answer questions clearly and concisely. Be conversational but informative.',
        tools: [],
        sub_agents: [],
        position: { x: 50, y: 150 },
      }
    },
    edges: [
      { from: 'START', to: 'chat_agent' },
      { from: 'chat_agent', to: 'END' },
    ]
  },
  {
    id: 'research_pipeline',
    name: 'Research Pipeline',
    icon: '🔍',
    description: 'Sequential: Researcher → Summarizer',
    agents: {
      'researcher': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a research specialist. Given a topic, search for comprehensive information using Google Search. Gather key facts, statistics, recent developments, and expert opinions. Present your findings in a structured format.',
        tools: ['google_search'],
        sub_agents: [],
        position: { x: 0, y: 0 },
      },
      'summarizer': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are an expert summarizer. Take the research findings and create a clear, concise summary with: 1) Key takeaways (3-5 bullet points), 2) Main findings, 3) Conclusions. Make it easy to understand for a general audience.',
        tools: [],
        sub_agents: [],
        position: { x: 0, y: 0 },
      },
      'research_pipeline': {
        type: 'sequential',
        instruction: '',
        tools: [],
        sub_agents: ['researcher', 'summarizer'],
        position: { x: 50, y: 150 },
      }
    },
    edges: [
      { from: 'START', to: 'research_pipeline' },
      { from: 'research_pipeline', to: 'END' },
    ]
  },
  {
    id: 'content_refiner',
    name: 'Content Refiner',
    icon: '✨',
    description: 'Loop agent that iteratively improves content',
    agents: {
      'improver': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a content editor. Review the text and improve it by: fixing grammar and spelling errors, enhancing clarity and flow, improving word choice, and strengthening the overall structure. Output the improved version.',
        tools: [],
        sub_agents: [],
        position: { x: 0, y: 0 },
      },
      'reviewer': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a quality reviewer. Evaluate the content for: clarity, grammar, flow, and completeness. If the content is polished and ready (score 8/10 or higher), call exit_loop. Otherwise, briefly note what still needs improvement and let the improver continue.',
        tools: ['exit_loop'],
        sub_agents: [],
        position: { x: 0, y: 0 },
      },
      'content_refiner': {
        type: 'loop',
        instruction: '',
        tools: [],
        sub_agents: ['improver', 'reviewer'],
        position: { x: 50, y: 150 },
        max_iterations: 3,
      }
    },
    edges: [
      { from: 'START', to: 'content_refiner' },
      { from: 'content_refiner', to: 'END' },
    ]
  },
  {
    id: 'parallel_analyzer',
    name: 'Parallel Analyzer',
    icon: '⚡',
    description: 'Run multiple analyses concurrently',
    agents: {
      'sentiment_analyzer': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'Analyze the sentiment of the provided text. Identify: 1) Overall sentiment (positive/negative/neutral with confidence %), 2) Emotional tones present (joy, anger, sadness, etc.), 3) Key phrases that indicate sentiment. Format as a brief report.',
        tools: [],
        sub_agents: [],
        position: { x: 0, y: 0 },
      },
      'entity_extractor': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'Extract key entities from the text. Identify and categorize: 1) People (names, roles), 2) Organizations (companies, institutions), 3) Locations (cities, countries), 4) Dates and times, 5) Key topics/concepts. Format as a structured list.',
        tools: [],
        sub_agents: [],
        position: { x: 0, y: 0 },
      },
      'parallel_analyzer': {
        type: 'parallel',
        instruction: '',
        tools: [],
        sub_agents: ['sentiment_analyzer', 'entity_extractor'],
        position: { x: 50, y: 150 },
      }
    },
    edges: [
      { from: 'START', to: 'parallel_analyzer' },
      { from: 'parallel_analyzer', to: 'END' },
    ]
  },
  {
    id: 'support_router',
    name: 'Support Router',
    icon: '🔀',
    description: 'Route requests to specialized agents',
    agents: {
      'router': {
        type: 'router',
        model: 'gemini-2.0-flash',
        instruction: 'Classify the user request into one category: "technical" for coding, bugs, API issues, or technical problems; "billing" for payments, subscriptions, refunds, or account charges; "general" for all other questions. Respond with just the category word.',
        tools: [],
        sub_agents: [],
        position: { x: 200, y: 100 },
        routes: [
          { condition: 'technical', target: 'tech_support' },
          { condition: 'billing', target: 'billing_support' },
          { condition: 'general', target: 'general_support' },
        ],
      },
      'tech_support': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a senior technical support engineer. Help users with coding issues, bugs, API problems, and technical troubleshooting. Ask clarifying questions if needed. Provide code examples when helpful. Be patient and thorough.',
        tools: [],
        sub_agents: [],
        position: { x: 50, y: 350 },
      },
      'billing_support': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a billing specialist. Help users with payment issues, subscription questions, refund requests, and account billing inquiries. Be empathetic and solution-oriented. Explain charges clearly.',
        tools: [],
        sub_agents: [],
        position: { x: 200, y: 350 },
      },
      'general_support': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a friendly general support agent. Help users with general questions, product information, feature requests, and any inquiries that don\'t fit technical or billing categories. Be helpful and personable.',
        tools: [],
        sub_agents: [],
        position: { x: 350, y: 350 },
      }
    },
    edges: [
      { from: 'START', to: 'router' },
      { from: 'router', to: 'tech_support' },
      { from: 'router', to: 'billing_support' },
      { from: 'router', to: 'general_support' },
      { from: 'tech_support', to: 'END' },
      { from: 'billing_support', to: 'END' },
      { from: 'general_support', to: 'END' },
    ]
  },
  {
    id: 'web_researcher',
    name: 'Web Researcher',
    icon: '🌐',
    description: 'Agent that browses the web for information',
    agents: {
      'web_agent': {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a web research assistant with browser capabilities. When asked a question: 1) Navigate to relevant websites to find accurate, up-to-date information, 2) Read and extract key content from pages, 3) Synthesize findings into a clear answer with sources. Always cite your sources.',
        tools: ['browser'],
        sub_agents: [],
        position: { x: 50, y: 150 },
      }
    },
    edges: [
      { from: 'START', to: 'web_agent' },
      { from: 'web_agent', to: 'END' },
    ]
  },
];
