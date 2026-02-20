import { useState, useEffect, useCallback } from 'react';
import { api } from '../api/client';

/**
 * Provider metadata for known API key names.
 */
interface ProviderMeta {
  provider: string;
  providerName: string;
  providerIcon: string;
}

const PROVIDER_METADATA: Record<string, ProviderMeta> = {
  GOOGLE_API_KEY: { provider: 'gemini', providerName: 'Google Gemini', providerIcon: '✨' },
  GEMINI_API_KEY: { provider: 'gemini', providerName: 'Google Gemini', providerIcon: '✨' },
  OPENAI_API_KEY: { provider: 'openai', providerName: 'OpenAI', providerIcon: '🤖' },
  ANTHROPIC_API_KEY: { provider: 'anthropic', providerName: 'Anthropic Claude', providerIcon: '🎭' },
  DEEPSEEK_API_KEY: { provider: 'deepseek', providerName: 'DeepSeek', providerIcon: '🔍' },
  GROQ_API_KEY: { provider: 'groq', providerName: 'Groq', providerIcon: '⚡' },
  OLLAMA_HOST: { provider: 'ollama', providerName: 'Ollama (Local)', providerIcon: '🦙' },
  GITHUB_TOKEN: { provider: 'github', providerName: 'GitHub', providerIcon: '🐙' },
  SLACK_BOT_TOKEN: { provider: 'slack', providerName: 'Slack', providerIcon: '💬' },
};

/**
 * Status of a single provider key, enriched with provider metadata.
 */
export interface ProviderKeyStatus {
  name: string;
  provider: string;
  providerName: string;
  providerIcon: string;
  source: 'environment' | 'project' | 'not_set';
  masked: string | null;
  /** True when this key is also detected in the environment (relevant for project-level overrides). */
  environmentDetected: boolean;
}

/**
 * Hook for managing API keys for a project.
 *
 * Fetches project keys on mount (when projectId is provided), maps them to
 * ProviderKeyStatus objects with provider metadata, and exposes save/remove/refresh actions.
 *
 * @param projectId - The project UUID, or null if no project is selected.
 */
export function useApiKeys(projectId: string | null) {
  const [keys, setKeys] = useState<ProviderKeyStatus[]>([]);
  const [isLoading, setIsLoading] = useState(false);

  const fetchKeys = useCallback(async () => {
    if (!projectId) {
      setKeys([]);
      return;
    }
    setIsLoading(true);
    try {
      const [projectResponse, detectedResponse] = await Promise.all([
        api.keys.getProjectKeys(projectId),
        api.settings.getDetectedKeys(),
      ]);

      // Build a set of key names detected in the environment
      const envDetectedNames = new Set(
        detectedResponse.keys
          .filter(k => k.status === 'detected')
          .map(k => k.name),
      );

      const mapped = projectResponse.keys.map((entry): ProviderKeyStatus => {
        const meta = PROVIDER_METADATA[entry.name];
        return {
          name: entry.name,
          provider: meta?.provider ?? entry.name.toLowerCase(),
          providerName: meta?.providerName ?? entry.name,
          providerIcon: meta?.providerIcon ?? '🔑',
          source: entry.source,
          masked: entry.masked,
          environmentDetected: envDetectedNames.has(entry.name),
        };
      });
      setKeys(mapped);
    } catch {
      // Keep existing keys on error so the UI doesn't flash empty
    } finally {
      setIsLoading(false);
    }
  }, [projectId]);

  useEffect(() => {
    fetchKeys();
  }, [fetchKeys]);

  const saveKey = useCallback(
    async (name: string, value: string) => {
      if (!projectId) return;
      await api.keys.saveProjectKeys(projectId, { [name]: value });
      await fetchKeys();
    },
    [projectId, fetchKeys],
  );

  const removeKey = useCallback(
    async (name: string) => {
      if (!projectId) return;
      await api.keys.deleteProjectKey(projectId, name);
      await fetchKeys();
    },
    [projectId, fetchKeys],
  );

  return { keys, isLoading, saveKey, removeKey, refresh: fetchKeys };
}
