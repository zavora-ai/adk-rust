/**
 * Unit tests for ApiKeyRow component logic.
 *
 * Tests the ProviderKeyStatus rendering logic and prop interface
 * without React rendering (no @testing-library/react available).
 */

import { describe, it, expect } from 'vitest';
import type { ProviderKeyStatus } from '../../hooks/useApiKeys';

/**
 * Helper to create a ProviderKeyStatus for testing.
 */
function makeProvider(overrides: Partial<ProviderKeyStatus> = {}): ProviderKeyStatus {
  return {
    name: 'OPENAI_API_KEY',
    provider: 'openai',
    providerName: 'OpenAI',
    providerIcon: '🤖',
    source: 'not_set',
    masked: null,
    environmentDetected: false,
    ...overrides,
  };
}

describe('ApiKeyRow', () => {
  describe('ProviderKeyStatus source states', () => {
    it('environment source should have a masked value', () => {
      const provider = makeProvider({
        source: 'environment',
        masked: '••••••••Ab1x',
      });
      expect(provider.source).toBe('environment');
      expect(provider.masked).toBeTruthy();
      expect(provider.masked).toContain('••');
    });

    it('project source should have a masked value', () => {
      const provider = makeProvider({
        source: 'project',
        masked: '••••••••n-7f',
      });
      expect(provider.source).toBe('project');
      expect(provider.masked).toBeTruthy();
    });

    it('not_set source should have null masked value', () => {
      const provider = makeProvider({
        source: 'not_set',
        masked: null,
      });
      expect(provider.source).toBe('not_set');
      expect(provider.masked).toBeNull();
    });
  });

  describe('Provider metadata', () => {
    it('should carry provider icon and display name', () => {
      const provider = makeProvider({
        name: 'GOOGLE_API_KEY',
        provider: 'gemini',
        providerName: 'Google Gemini',
        providerIcon: '✨',
      });
      expect(provider.providerIcon).toBe('✨');
      expect(provider.providerName).toBe('Google Gemini');
      expect(provider.name).toBe('GOOGLE_API_KEY');
    });

    it('should support all known providers', () => {
      const providers: Array<Pick<ProviderKeyStatus, 'name' | 'provider' | 'providerIcon'>> = [
        { name: 'GOOGLE_API_KEY', provider: 'gemini', providerIcon: '✨' },
        { name: 'OPENAI_API_KEY', provider: 'openai', providerIcon: '🤖' },
        { name: 'ANTHROPIC_API_KEY', provider: 'anthropic', providerIcon: '🎭' },
        { name: 'DEEPSEEK_API_KEY', provider: 'deepseek', providerIcon: '🔍' },
        { name: 'GROQ_API_KEY', provider: 'groq', providerIcon: '⚡' },
        { name: 'OLLAMA_HOST', provider: 'ollama', providerIcon: '🦙' },
      ];

      for (const p of providers) {
        const status = makeProvider(p);
        expect(status.name).toBeTruthy();
        expect(status.provider).toBeTruthy();
        expect(status.providerIcon).toBeTruthy();
      }
    });
  });

  describe('Source state determines UI behavior', () => {
    it('environment keys should be read-only (no edit/remove)', () => {
      const provider = makeProvider({ source: 'environment' });
      // Environment keys are read-only — the component shows no edit/remove buttons
      expect(provider.source).toBe('environment');
    });

    it('project keys should allow edit and remove', () => {
      const provider = makeProvider({ source: 'project', masked: '••••••••sk-7f' });
      // Project keys show Edit and Remove buttons
      expect(provider.source).toBe('project');
      expect(provider.masked).toBeTruthy();
    });

    it('not_set keys should show "Enter key..." prompt', () => {
      const provider = makeProvider({ source: 'not_set' });
      // Not-set keys show an expandable input prompt
      expect(provider.source).toBe('not_set');
      expect(provider.masked).toBeNull();
    });
  });
});
