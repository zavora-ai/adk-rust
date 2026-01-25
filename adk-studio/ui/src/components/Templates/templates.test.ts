/**
 * Tests for template loading and validation
 * 
 * Checkpoint 21: Verify templates load correctly
 * 
 * Requirements: 6.1, 6.2, 6.3, 6.4, 6.7
 */

import { describe, it, expect } from 'vitest';
import * as fc from 'fast-check';
import { 
  TEMPLATES, 
  getTemplateById, 
  getTemplatesByCategory, 
  getCategories,
  CATEGORY_LABELS,
  type TemplateCategory 
} from './templates';

describe('Template Gallery - Checkpoint 21 Verification', () => {
  /**
   * Requirement 6.1: System SHALL provide 8-12 curated agent workflow templates
   */
  describe('Template Count', () => {
    it('should have between 8 and 12 curated templates', () => {
      expect(TEMPLATES.length).toBeGreaterThanOrEqual(8);
      expect(TEMPLATES.length).toBeLessThanOrEqual(12);
    });
  });

  /**
   * Requirement 6.2: Templates SHALL include agent teams, eval loops, tool-heavy, realtime
   */
  describe('Template Categories', () => {
    it('should include all required categories', () => {
      const categories = new Set(TEMPLATES.map(t => t.category));
      
      // Must have basic, advanced, tools, teams, realtime
      expect(categories.has('basic')).toBe(true);
      expect(categories.has('advanced')).toBe(true);
      expect(categories.has('tools')).toBe(true);
      expect(categories.has('teams')).toBe(true);
      expect(categories.has('realtime')).toBe(true);
    });

    it('should have at least one template per category', () => {
      const categories = getCategories();
      
      for (const category of categories) {
        const templatesInCategory = getTemplatesByCategory(category);
        expect(templatesInCategory.length).toBeGreaterThanOrEqual(1);
      }
    });
  });

  /**
   * Requirement 6.7: Template gallery SHALL display name, description, and preview
   */
  describe('Template Structure', () => {
    it('each template should have required fields', () => {
      for (const template of TEMPLATES) {
        // Required fields
        expect(template.id).toBeDefined();
        expect(typeof template.id).toBe('string');
        expect(template.id.length).toBeGreaterThan(0);

        expect(template.name).toBeDefined();
        expect(typeof template.name).toBe('string');
        expect(template.name.length).toBeGreaterThan(0);

        expect(template.description).toBeDefined();
        expect(typeof template.description).toBe('string');
        expect(template.description.length).toBeGreaterThan(0);

        expect(template.icon).toBeDefined();
        expect(typeof template.icon).toBe('string');
        expect(template.icon.length).toBeGreaterThan(0);

        expect(template.category).toBeDefined();
        expect(['basic', 'advanced', 'tools', 'teams', 'realtime']).toContain(template.category);

        expect(template.agents).toBeDefined();
        expect(typeof template.agents).toBe('object');

        expect(template.edges).toBeDefined();
        expect(Array.isArray(template.edges)).toBe(true);
      }
    });

    it('each template should have unique id', () => {
      const ids = TEMPLATES.map(t => t.id);
      const uniqueIds = new Set(ids);
      expect(uniqueIds.size).toBe(ids.length);
    });
  });

  /**
   * Requirement 6.3: Each template SHALL have a "Run" button for immediate execution
   * Requirement 6.4: Each template SHALL be editable after loading
   */
  describe('Template Agents', () => {
    it('each template should have at least one agent', () => {
      for (const template of TEMPLATES) {
        const agentCount = Object.keys(template.agents).length;
        expect(agentCount).toBeGreaterThanOrEqual(1);
      }
    });

    it('each agent should have required fields for editability', () => {
      for (const template of TEMPLATES) {
        for (const [agentId, agent] of Object.entries(template.agents)) {
          expect(agentId).toBeDefined();
          expect(typeof agentId).toBe('string');
          
          expect(agent.type).toBeDefined();
          expect(['llm', 'sequential', 'parallel', 'loop', 'router']).toContain(agent.type);
          
          expect(agent.instruction).toBeDefined();
          expect(typeof agent.instruction).toBe('string');
          
          expect(agent.tools).toBeDefined();
          expect(Array.isArray(agent.tools)).toBe(true);
          
          expect(agent.sub_agents).toBeDefined();
          expect(Array.isArray(agent.sub_agents)).toBe(true);
        }
      }
    });
  });

  /**
   * Template edges should be valid
   */
  describe('Template Edges', () => {
    it('each template should have valid edge connections', () => {
      for (const template of TEMPLATES) {
        for (const edge of template.edges) {
          expect(edge.from).toBeDefined();
          expect(typeof edge.from).toBe('string');
          
          expect(edge.to).toBeDefined();
          expect(typeof edge.to).toBe('string');
          
          // Edge endpoints should be START, END, or a valid agent
          const validEndpoints = ['START', 'END', ...Object.keys(template.agents)];
          expect(validEndpoints).toContain(edge.from);
          expect(validEndpoints).toContain(edge.to);
        }
      }
    });

    it('each template should have START and END connections', () => {
      for (const template of TEMPLATES) {
        const hasStartEdge = template.edges.some(e => e.from === 'START');
        const hasEndEdge = template.edges.some(e => e.to === 'END');
        
        expect(hasStartEdge).toBe(true);
        expect(hasEndEdge).toBe(true);
      }
    });
  });

  /**
   * Template retrieval functions
   */
  describe('Template Retrieval', () => {
    it('getTemplateById should return correct template', () => {
      for (const template of TEMPLATES) {
        const retrieved = getTemplateById(template.id);
        expect(retrieved).toBeDefined();
        expect(retrieved?.id).toBe(template.id);
        expect(retrieved?.name).toBe(template.name);
      }
    });

    it('getTemplateById should return undefined for invalid id', () => {
      const retrieved = getTemplateById('non_existent_template_id');
      expect(retrieved).toBeUndefined();
    });

    it('getTemplatesByCategory should filter correctly', () => {
      const categories = getCategories();
      
      for (const category of categories) {
        const filtered = getTemplatesByCategory(category);
        
        // All returned templates should be in the requested category
        for (const template of filtered) {
          expect(template.category).toBe(category);
        }
      }
    });

    it('getTemplatesByCategory("all") should return all templates', () => {
      const all = getTemplatesByCategory('all');
      expect(all.length).toBe(TEMPLATES.length);
    });
  });

  /**
   * Category labels
   */
  describe('Category Labels', () => {
    it('should have labels for all categories', () => {
      const categories = getCategories();
      
      for (const category of categories) {
        expect(CATEGORY_LABELS[category]).toBeDefined();
        expect(typeof CATEGORY_LABELS[category]).toBe('string');
        expect(CATEGORY_LABELS[category].length).toBeGreaterThan(0);
      }
    });

    it('should have label for "all" category', () => {
      expect(CATEGORY_LABELS.all).toBeDefined();
      expect(typeof CATEGORY_LABELS.all).toBe('string');
    });
  });

  /**
   * Property-based test: Template loading round-trip
   */
  describe('Property Tests', () => {
    /**
     * Property: For any template, loading by ID should return the same template
     */
    it('should retrieve any template by its ID correctly', () => {
      fc.assert(
        fc.property(
          fc.integer({ min: 0, max: TEMPLATES.length - 1 }),
          (index: number) => {
            const template = TEMPLATES[index];
            const retrieved = getTemplateById(template.id);
            
            expect(retrieved).toBeDefined();
            expect(retrieved?.id).toBe(template.id);
            expect(retrieved?.name).toBe(template.name);
            expect(retrieved?.description).toBe(template.description);
            expect(retrieved?.category).toBe(template.category);
            expect(Object.keys(retrieved?.agents || {}).length).toBe(Object.keys(template.agents).length);
            expect(retrieved?.edges.length).toBe(template.edges.length);
          }
        ),
        { numRuns: 100 }
      );
    });

    /**
     * Property: Category filtering should be consistent
     */
    it('should filter templates by category consistently', () => {
      const arbCategory = fc.constantFrom<TemplateCategory | 'all'>('all', 'basic', 'advanced', 'tools', 'teams', 'realtime');
      
      fc.assert(
        fc.property(arbCategory, (category: TemplateCategory | 'all') => {
          const filtered = getTemplatesByCategory(category);
          
          if (category === 'all') {
            expect(filtered.length).toBe(TEMPLATES.length);
          } else {
            // All filtered templates should match the category
            for (const template of filtered) {
              expect(template.category).toBe(category);
            }
            
            // Count should match manual filter
            const manualCount = TEMPLATES.filter(t => t.category === category).length;
            expect(filtered.length).toBe(manualCount);
          }
        }),
        { numRuns: 100 }
      );
    });
  });
});
