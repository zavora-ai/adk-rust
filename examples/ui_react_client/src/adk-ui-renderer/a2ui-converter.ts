import type { Component } from './types';

/**
 * Converts A2UI v0.9 nested component format to flat format expected by Renderer
 * 
 * A2UI format: { id: "x", component: { Text: { text: { literalString: "hello" } } } }
 * Flat format: { type: "text", id: "x", content: "hello" }
 */

interface A2UIComponent {
  id: string;
  component: Record<string, any>;
}

function extractText(textField: any): string {
  if (typeof textField === 'string') return textField;
  if (textField?.literalString) return textField.literalString;
  if (textField?.dynamicString) return textField.dynamicString;
  return '';
}

function normalizeGap(gap: any, spacing: any): number | string | undefined {
  if (typeof gap === 'number' || typeof gap === 'string') {
    return gap;
  }

  if (typeof spacing !== 'string') {
    return undefined;
  }

  const token = spacing.trim().toLowerCase();
  if (!token) {
    return undefined;
  }

  const tokenMap: Record<string, string> = {
    none: '0px',
    xs: '4px',
    sm: '8px',
    small: '8px',
    md: '12px',
    medium: '12px',
    lg: '16px',
    large: '16px',
    xl: '24px',
    xlarge: '24px',
    '2xl': '32px',
  };

  return tokenMap[token] ?? spacing;
}

function normalizeNestedEntries(entries: any): any[] {
  if (!Array.isArray(entries)) {
    return [];
  }

  return entries
    .map((entry) => {
      if (typeof entry === 'string') {
        return entry;
      }
      return convertA2UIComponent(entry);
    })
    .filter((entry) => entry !== null && entry !== undefined);
}

export function convertA2UIComponent(a2ui: A2UIComponent): Component | null {
  const { id, component } = a2ui;
  
  if (!component || typeof component !== 'object') {
    console.error('Invalid component:', a2ui);
    return null;
  }
  
  const entries = Object.entries(component);
  if (entries.length === 0) {
    console.error('Empty component object:', a2ui);
    return null;
  }
  
  const [componentType, props] = entries[0];

  switch (componentType) {
    case 'Text':
      return {
        type: 'text',
        id,
        content: extractText(props.text),
        variant: props.variant as any,
      };

    case 'Button':
      return {
        type: 'button',
        id,
        label: extractText(props.label || props.text),
        action_id: props.actionId || props.action?.event?.name || id,
        variant: props.variant as any,
        disabled: props.disabled,
      };

    case 'Image':
      return {
        type: 'image',
        id,
        src: extractText(props.url || props.src),
        alt: props.alt ? extractText(props.alt) : undefined,
      };

    case 'Icon':
      return {
        type: 'icon',
        id,
        name: props.name,
        size: props.size,
      };

    case 'Badge':
      return {
        type: 'badge',
        id,
        label: extractText(props.label),
        variant: props.variant as any,
      };

    case 'Divider':
      return { type: 'divider', id };

    case 'Input':
    case 'TextInput':
    case 'TextField':
      return {
        type: 'text_input',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        input_type: props.inputType || props.type || 'text',
        placeholder: props.placeholder ? extractText(props.placeholder) : undefined,
        required: props.required,
        default_value: props.defaultValue ? extractText(props.defaultValue) : undefined,
      };

    case 'TextArea':
    case 'Textarea':
      return {
        type: 'textarea',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        placeholder: props.placeholder ? extractText(props.placeholder) : undefined,
        required: props.required,
        rows: props.rows || 4,
        default_value: props.defaultValue ? extractText(props.defaultValue) : undefined,
      };

    case 'NumberInput':
      return {
        type: 'number_input',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        min: props.min,
        max: props.max,
        step: props.step,
        required: props.required,
        default_value: props.defaultValue,
      };

    case 'Select':
      return {
        type: 'select',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        options: (props.options || []).map((opt: any) => ({
          value: typeof opt === 'string' ? opt : (opt.value || extractText(opt.label) || opt),
          label: typeof opt === 'string' ? opt : extractText(opt.label || opt.value || opt),
        })),
        required: props.required,
      };

    case 'MultiSelect':
      return {
        type: 'multi_select',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        options: (props.options || []).map((opt: any) => ({
          value: typeof opt === 'string' ? opt : (opt.value || extractText(opt.label) || opt),
          label: typeof opt === 'string' ? opt : extractText(opt.label || opt.value || opt),
        })),
        required: props.required,
      };

    case 'Switch':
    case 'CheckBox':
      return {
        type: 'switch',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        default_checked: props.defaultChecked,
      };

    case 'DateInput':
    case 'DateTimeInput':
      return {
        type: 'date_input',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        required: props.required,
      };

    case 'Slider':
      return {
        type: 'slider',
        id,
        name: props.name || id,
        label: props.label ? extractText(props.label) : '',
        min: props.min,
        max: props.max,
        step: props.step,
        default_value: props.defaultValue,
      };

    case 'Column':
      return {
        type: 'stack',
        id,
        direction: 'vertical',
        children: props.children || [],
        gap: normalizeGap(props.gap, props.spacing),
        justify: props.justify,
        align: props.align,
        wrap: props.wrap,
      };

    case 'Row':
      return {
        type: 'stack',
        id,
        direction: 'horizontal',
        children: props.children || [],
        gap: normalizeGap(props.gap, props.spacing),
        justify: props.justify,
        align: props.align,
        wrap: props.wrap,
      };

    case 'Grid':
      return {
        type: 'grid',
        id,
        columns: props.columns || 2,
        children: props.children || [],
        gap: normalizeGap(props.gap, props.spacing),
      };

    case 'Card':
      return {
        type: 'card',
        id,
        title: props.title ? extractText(props.title) : undefined,
        description: props.description ? extractText(props.description) : undefined,
        content: normalizeNestedEntries(props.content) as any,
        footer: normalizeNestedEntries(props.footer) as any,
      };

    case 'Container':
      return {
        type: 'container',
        id,
        children: normalizeNestedEntries(props.children) as any,
        padding: props.padding,
      };

    case 'Tabs':
      return {
        type: 'tabs',
        id,
        tabs: props.tabs || [],
      };

    case 'Table':
      return {
        type: 'table',
        id,
        columns: props.columns || [],
        data: props.data || [],
        sortable: props.sortable,
        page_size: props.pageSize,
        striped: props.striped,
      };

    case 'TableColumn':
      // Column definitions are consumed by the higher-level surface parser and
      // hydrated into Table.columns there. They are not standalone render nodes.
      return null;

    case 'List':
      return {
        type: 'list',
        id,
        items: props.items || [],
        ordered: props.ordered,
      };

    case 'KeyValue':
      return {
        type: 'key_value',
        id,
        pairs: props.pairs || [],
      };

    case 'CodeBlock':
      return {
        type: 'code_block',
        id,
        code: extractText(props.code),
        language: props.language,
      };

    case 'Chart':
      return {
        type: 'chart',
        id,
        title: props.title ? extractText(props.title) : undefined,
        kind: props.kind,
        data: props.data || [],
        x_key: props.xKey,
        y_keys: props.yKeys || [],
        x_label: props.xLabel,
        y_label: props.yLabel,
        show_legend: props.showLegend,
        colors: props.colors,
      };

    case 'Alert':
      return {
        type: 'alert',
        id,
        title: extractText(props.title),
        description: props.description ? extractText(props.description) : undefined,
        variant: props.variant as any,
      };

    case 'Progress':
      return {
        type: 'progress',
        id,
        value: props.value || 0,
        label: props.label ? extractText(props.label) : undefined,
      };

    case 'Toast':
      return {
        type: 'toast',
        id,
        message: extractText(props.message),
        variant: props.variant as any,
        duration: props.duration,
        dismissible: props.dismissible,
      };

    case 'Modal':
      return {
        type: 'modal',
        id,
        title: extractText(props.title),
        content: normalizeNestedEntries(props.content) as any,
        footer: normalizeNestedEntries(props.footer) as any,
        size: props.size as any,
        closable: props.closable,
      };

    case 'Spinner':
      return {
        type: 'spinner',
        id,
        size: props.size as any,
        label: props.label ? extractText(props.label) : undefined,
      };

    case 'Skeleton':
      return {
        type: 'skeleton',
        id,
        variant: props.variant as any,
        width: props.width,
        height: props.height,
      };

    default:
      console.warn(`Unknown A2UI component type: ${componentType}`);
      return null;
  }
}

export function convertA2UIMessage(message: any): Component[] {
  // Handle A2UI v0.9 format
  if (message.components && Array.isArray(message.components)) {
    const firstComp = message.components[0];
    
    // Check if it's nested A2UI format
    if (firstComp?.component && typeof firstComp.component === 'object') {
      return message.components
        .map(convertA2UIComponent)
        .filter((c: Component | null): c is Component => c !== null);
    }
    
    // Already flat format
    return message.components;
  }

  return [];
}
