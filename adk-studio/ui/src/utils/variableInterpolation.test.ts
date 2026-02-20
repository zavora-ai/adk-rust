/**
 * Property Tests for HTTP Variable Interpolation
 * 
 * **Feature: action-nodes, Property 3: HTTP Variable Interpolation**
 * *For any* valid URL template with {{variable}} placeholders and a state object,
 * variable interpolation SHALL correctly substitute all variables with their values.
 * **Validates: Requirements 3.1**
 */

import { describe, it, expect } from 'vitest';
import * as fc from 'fast-check';
import {
  hasVariables,
  extractVariables,
  getNestedValue,
  valueToString,
  interpolateVariables,
  validateVariables,
  previewInterpolation,
  // VARIABLE_PATTERN is exported for use in other modules
} from './variableInterpolation';

// ============================================
// Arbitraries (Generators)
// ============================================

/**
 * Generator for valid variable names (alphanumeric with underscores).
 */
const arbVariableName: fc.Arbitrary<string> = fc
  .stringMatching(/^[a-zA-Z_][a-zA-Z0-9_]*$/)
  .filter((s) => s.length >= 1 && s.length <= 30);

/**
 * Generator for nested variable paths (e.g., "user.profile.name").
 */
const arbNestedPath: fc.Arbitrary<string> = fc
  .array(arbVariableName, { minLength: 1, maxLength: 4 })
  .map((parts) => parts.join('.'));

/**
 * Generator for variable placeholder strings.
 */
const arbVariablePlaceholder: fc.Arbitrary<string> = arbNestedPath.map(
  (path) => `{{${path}}}`
);

/**
 * Generator for simple string values (no special characters that would break URLs).
 */
const arbSimpleValue: fc.Arbitrary<string> = fc.stringMatching(/^[a-zA-Z0-9_-]*$/);

/**
 * Generator for primitive values that can be interpolated.
 */
const arbPrimitiveValue: fc.Arbitrary<string | number | boolean> = fc.oneof(
  arbSimpleValue,
  fc.integer({ min: -1000000, max: 1000000 }),
  fc.boolean()
);

/**
 * Generator for state objects with simple key-value pairs.
 */
const arbSimpleState: fc.Arbitrary<Record<string, string | number | boolean>> = fc.dictionary(
  arbVariableName,
  arbPrimitiveValue
);

/**
 * Generator for URL-like templates with variable placeholders.
 */
const arbUrlTemplate: fc.Arbitrary<string> = fc
  .tuple(
    fc.constantFrom('https://', 'http://'),
    fc.stringMatching(/^[a-z0-9-]+\.[a-z]{2,4}$/),
    fc.array(
      fc.oneof(
        fc.stringMatching(/^[a-z0-9-]+$/),
        arbVariablePlaceholder
      ),
      { minLength: 0, maxLength: 5 }
    )
  )
  .map(([protocol, domain, pathParts]) => {
    const path = pathParts.length > 0 ? '/' + pathParts.join('/') : '';
    return `${protocol}${domain}${path}`;
  });

// ============================================
// Property Tests
// ============================================

describe('Variable Interpolation', () => {
  describe('Property 3: HTTP Variable Interpolation', () => {
    /**
     * **Property 3.1: Variable Detection**
     * *For any* string containing {{variable}} patterns, hasVariables SHALL return true.
     * *For any* string without {{variable}} patterns, hasVariables SHALL return false.
     */
    it('should correctly detect presence of variables', () => {
      fc.assert(
        fc.property(arbVariablePlaceholder, (placeholder) => {
          expect(hasVariables(placeholder)).toBe(true);
        }),
        { numRuns: 100 }
      );

      fc.assert(
        fc.property(arbSimpleValue, (value) => {
          // Simple values without {{ }} should not have variables
          if (!value.includes('{{') && !value.includes('}}')) {
            expect(hasVariables(value)).toBe(false);
          }
        }),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.2: Variable Extraction**
     * *For any* template with N unique variable placeholders, extractVariables
     * SHALL return exactly N unique variable names.
     */
    it('should extract all unique variable names from template', () => {
      fc.assert(
        fc.property(
          fc.array(arbNestedPath, { minLength: 1, maxLength: 5 }),
          (paths) => {
            const uniquePaths = [...new Set(paths)];
            const template = uniquePaths.map((p) => `{{${p}}}`).join(' ');
            const extracted = extractVariables(template);

            expect(extracted.length).toBe(uniquePaths.length);
            for (const path of uniquePaths) {
              expect(extracted).toContain(path);
            }
          }
        ),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.3: Simple Variable Substitution**
     * *For any* template with a single variable and matching state value,
     * interpolation SHALL replace the placeholder with the value.
     */
    it('should substitute single variable with state value', () => {
      fc.assert(
        fc.property(
          arbVariableName,
          arbPrimitiveValue,
          (varName, value) => {
            const template = `prefix_{{${varName}}}_suffix`;
            const state = { [varName]: value };
            const result = interpolateVariables(template, state);

            const expectedValue = valueToString(value);
            expect(result).toBe(`prefix_${expectedValue}_suffix`);
          }
        ),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.4: Nested Path Resolution**
     * *For any* nested path and corresponding nested state object,
     * interpolation SHALL correctly resolve the nested value.
     */
    it('should resolve nested variable paths', () => {
      fc.assert(
        fc.property(
          fc.array(arbVariableName, { minLength: 2, maxLength: 4 }),
          arbPrimitiveValue,
          (pathParts, value) => {
            const path = pathParts.join('.');
            const template = `{{${path}}}`;

            // Build nested state object
            const state: Record<string, unknown> = {};
            let current = state;
            for (let i = 0; i < pathParts.length - 1; i++) {
              current[pathParts[i]] = {};
              current = current[pathParts[i]] as Record<string, unknown>;
            }
            current[pathParts[pathParts.length - 1]] = value;

            const result = interpolateVariables(template, state);
            expect(result).toBe(valueToString(value));
          }
        ),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.5: Missing Variable Handling**
     * *For any* template with variables not present in state,
     * interpolation SHALL replace missing variables with empty string.
     */
    it('should replace missing variables with empty string', () => {
      fc.assert(
        fc.property(arbVariableName, (varName) => {
          const template = `prefix_{{${varName}}}_suffix`;
          const state = {}; // Empty state - variable is missing
          const result = interpolateVariables(template, state);

          expect(result).toBe('prefix__suffix');
        }),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.6: Multiple Variable Substitution**
     * *For any* template with multiple variables and matching state,
     * interpolation SHALL replace all variables correctly.
     */
    it('should substitute multiple variables correctly', () => {
      fc.assert(
        fc.property(
          fc.array(
            fc.tuple(arbVariableName, arbPrimitiveValue),
            { minLength: 2, maxLength: 5 }
          ),
          (pairs) => {
            // Ensure unique variable names
            const uniquePairs = pairs.filter(
              (pair, index, self) =>
                self.findIndex((p) => p[0] === pair[0]) === index
            );

            if (uniquePairs.length < 2) return; // Skip if not enough unique pairs

            const template = uniquePairs.map(([name]) => `{{${name}}}`).join('/');
            const state = Object.fromEntries(uniquePairs);
            const result = interpolateVariables(template, state);

            const expected = uniquePairs
              .map(([, value]) => valueToString(value))
              .join('/');
            expect(result).toBe(expected);
          }
        ),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.7: Idempotence Without Variables**
     * *For any* string without variable placeholders,
     * interpolation SHALL return the original string unchanged.
     */
    it('should return original string when no variables present', () => {
      fc.assert(
        fc.property(
          fc.string().filter((s) => !s.includes('{{') && !s.includes('}}')),
          arbSimpleState,
          (template, state) => {
            const result = interpolateVariables(template, state);
            expect(result).toBe(template);
          }
        ),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.8: URL Template Interpolation**
     * *For any* URL template with variables and matching state,
     * interpolation SHALL produce a valid URL structure.
     */
    it('should correctly interpolate URL templates', () => {
      fc.assert(
        fc.property(arbUrlTemplate, arbSimpleState, (template, state) => {
          const result = interpolateVariables(template, state);

          // Result should still start with http:// or https://
          expect(result.startsWith('http://') || result.startsWith('https://')).toBe(
            true
          );

          // Result should not contain unresolved {{ }} patterns
          // (they should be replaced with values or empty strings)
          const unresolvedPattern = /\{\{[^}]+\}\}/;
          expect(unresolvedPattern.test(result)).toBe(false);
        }),
        { numRuns: 100 }
      );
    });

    /**
     * **Property 3.9: Validation Accuracy**
     * *For any* template and state, validateVariables SHALL correctly identify
     * all missing variables.
     */
    it('should accurately validate variable presence', () => {
      fc.assert(
        fc.property(
          fc.array(arbVariableName, { minLength: 1, maxLength: 5 }),
          fc.array(arbVariableName, { minLength: 0, maxLength: 3 }),
          arbPrimitiveValue,
          (templateVars, stateVars, value) => {
            const uniqueTemplateVars = [...new Set(templateVars)];
            const uniqueStateVars = [...new Set(stateVars)];

            const template = uniqueTemplateVars.map((v) => `{{${v}}}`).join(' ');
            const state = Object.fromEntries(
              uniqueStateVars.map((v) => [v, value])
            );

            const { valid, missing } = validateVariables(template, state);

            // Calculate expected missing variables
            const expectedMissing = uniqueTemplateVars.filter(
              (v) => !uniqueStateVars.includes(v)
            );

            expect(missing.length).toBe(expectedMissing.length);
            expect(valid).toBe(expectedMissing.length === 0);

            for (const m of expectedMissing) {
              expect(missing).toContain(m);
            }
          }
        ),
        { numRuns: 100 }
      );
    });
  });

  describe('Helper Functions', () => {
    /**
     * Test getNestedValue with various path depths.
     */
    it('should get nested values correctly', () => {
      const obj = {
        a: 1,
        b: {
          c: 2,
          d: {
            e: 3,
          },
        },
      };

      expect(getNestedValue(obj, 'a')).toBe(1);
      expect(getNestedValue(obj, 'b.c')).toBe(2);
      expect(getNestedValue(obj, 'b.d.e')).toBe(3);
      expect(getNestedValue(obj, 'x')).toBeUndefined();
      expect(getNestedValue(obj, 'b.x')).toBeUndefined();
    });

    /**
     * Test valueToString with various types.
     */
    it('should convert values to strings correctly', () => {
      expect(valueToString('hello')).toBe('hello');
      expect(valueToString(123)).toBe('123');
      expect(valueToString(true)).toBe('true');
      expect(valueToString(false)).toBe('false');
      expect(valueToString(null)).toBe('null');
      expect(valueToString(undefined)).toBe('');
      expect(valueToString({ a: 1 })).toBe('{"a":1}');
      expect(valueToString([1, 2, 3])).toBe('[1,2,3]');
    });

    /**
     * Test previewInterpolation shows resolved and missing variables.
     */
    it('should preview interpolation with markers', () => {
      const template = '{{found}} and {{missing}}';
      const state = { found: 'value' };
      const preview = previewInterpolation(template, state);

      expect(preview).toBe('[value] and [MISSING: missing]');
    });
  });

  describe('Edge Cases', () => {
    it('should handle empty template', () => {
      expect(interpolateVariables('', {})).toBe('');
      expect(hasVariables('')).toBe(false);
      expect(extractVariables('')).toEqual([]);
    });

    it('should handle null/undefined inputs gracefully', () => {
      expect(interpolateVariables(null as unknown as string, {})).toBe('');
      expect(interpolateVariables(undefined as unknown as string, {})).toBe('');
      expect(hasVariables(null as unknown as string)).toBe(false);
      expect(extractVariables(null as unknown as string)).toEqual([]);
    });

    it('should handle empty state object', () => {
      const template = '{{var1}}/{{var2}}';
      expect(interpolateVariables(template, {})).toBe('/');
    });

    it('should handle whitespace in variable names', () => {
      const template = '{{ spaced }}';
      const state = { spaced: 'value' };
      expect(interpolateVariables(template, state)).toBe('value');
    });

    it('should handle consecutive variables', () => {
      const template = '{{a}}{{b}}{{c}}';
      const state = { a: '1', b: '2', c: '3' };
      expect(interpolateVariables(template, state)).toBe('123');
    });
  });
});
