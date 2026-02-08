import { useEffect, useMemo, useState } from 'react';
import { Renderer as UiRenderer } from './adk-ui-renderer/Renderer';
import { convertA2UIComponent } from './adk-ui-renderer/a2ui-converter';
import { uiEventToMessage, type Component, type UiEvent } from './adk-ui-renderer/types';
import './App.css';

type UiProtocol = 'adk_ui' | 'a2ui' | 'ag_ui' | 'mcp_apps';

interface SurfaceSnapshot {
  surfaceId: string;
  components: Component[];
  dataModel: Record<string, unknown>;
}

interface StreamLogEvent {
  id: number;
  at: string;
  protocol: UiProtocol;
  kind: string;
  preview: string;
  raw: unknown;
}

interface ExampleTarget {
  id: string;
  name: string;
  description: string;
  port: number;
  prompts: string[];
}

interface ProtocolCapability {
  protocol: string;
  versions: string[];
  features: string[];
  deprecation?: {
    stage: string;
    announcedOn: string;
    sunsetTargetOn?: string;
    replacementProtocols: string[];
    note?: string;
  };
}

type TableColumnDef = {
  header: string;
  accessor_key: string;
  sortable?: boolean;
};

const EXAMPLES: ExampleTarget[] = [
  {
    id: 'ui_demo',
    name: 'UI Demo',
    description: 'General purpose multi-surface demo',
    port: 8080,
    prompts: [
      'Create a dashboard with three KPI cards and a trend chart.',
      'Design an onboarding form with progress, validation hints, and submit actions.',
      'Build an operations command center with alerts, table, and a confirmation modal.',
    ],
  },
  {
    id: 'ui_working_support',
    name: 'Support Intake',
    description: 'Ticket intake and triage workflows',
    port: 8080,
    prompts: [
      'Build a support intake flow with severity selector, timeline, and submit button.',
      'Show an incident response board with ownership, status, and next steps.',
      'Create a postmortem form with root-cause sections and action items.',
    ],
  },
  {
    id: 'ui_working_appointment',
    name: 'Appointments',
    description: 'Scheduling and availability workflows',
    port: 8080,
    prompts: [
      'Render an appointment booking experience with service cards and time slots.',
      'Create a multi-step reschedule workflow with confirmation state.',
      'Show a clinician schedule table with status badges and reminders.',
    ],
  },
  {
    id: 'ui_working_events',
    name: 'Events',
    description: 'Registration and agenda workflows',
    port: 8080,
    prompts: [
      'Build an event registration UI with ticket options and attendee details.',
      'Render an agenda timeline with speaker cards and room map summary.',
      'Design a networking dashboard with RSVP stats and waitlist table.',
    ],
  },
  {
    id: 'ui_working_facilities',
    name: 'Facilities',
    description: 'Maintenance requests and escalation',
    port: 8080,
    prompts: [
      'Create a facilities issue form with priority, location, and SLA warning.',
      'Show a maintenance dispatch board with progress, owners, and alerts.',
      'Render a work-order detail view with checklist and completion modal.',
    ],
  },
  {
    id: 'ui_working_inventory',
    name: 'Inventory',
    description: 'Stock monitoring and restock workflows',
    port: 8080,
    prompts: [
      'Build an inventory monitor with low-stock alerts and reorder actions.',
      'Create a replenishment form with quantity controls and approval summary.',
      'Render supplier performance cards plus a lead-time trend chart.',
    ],
  },
];

const PROTOCOLS: Array<{ id: UiProtocol; label: string; hint: string }> = [
  {
    id: 'adk_ui',
    label: 'Legacy ADK UI',
    hint: 'Backward-compatible profile (deprecated).',
  },
  {
    id: 'a2ui',
    label: 'A2UI',
    hint: 'Structured surface messages and component updates.',
  },
  {
    id: 'ag_ui',
    label: 'AG-UI',
    hint: 'Lifecycle event stream with custom surface payloads.',
  },
  {
    id: 'mcp_apps',
    label: 'MCP Apps',
    hint: 'Hosted UI resources with metadata and tool linkage.',
  },
];

const MAX_EVENT_LOG = 120;

function nowLabel(): string {
  return new Date().toLocaleTimeString('en-US', { hour12: false });
}

function makeSessionId(): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }
  // Fallback for environments without crypto.randomUUID — uses crypto.getRandomValues
  // for better randomness than Math.random().
  if (typeof crypto !== 'undefined' && typeof crypto.getRandomValues === 'function') {
    const bytes = new Uint8Array(16);
    crypto.getRandomValues(bytes);
    return `session-${Array.from(bytes).map(b => b.toString(16).padStart(2, '0')).join('')}`;
  }
  return `session-${Date.now()}-${Math.floor(Math.random() * 1_000_000)}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function pickField(record: Record<string, unknown>, ...keys: string[]): unknown {
  for (const key of keys) {
    if (key in record) {
      return record[key];
    }
  }
  return undefined;
}

function parseMaybeJson(value: unknown): unknown {
  if (typeof value !== 'string') {
    return value;
  }
  const trimmed = value.trim();
  if (!trimmed.startsWith('{') && !trimmed.startsWith('[')) {
    return value;
  }
  try {
    return JSON.parse(trimmed);
  } catch {
    return value;
  }
}

function normalizeProtocol(value: unknown): UiProtocol | null {
  if (typeof value !== 'string') {
    return null;
  }
  const normalized = value.trim().toLowerCase();
  if (normalized === 'adk_ui' || normalized === 'adk-ui') return 'adk_ui';
  if (normalized === 'a2ui') return 'a2ui';
  if (normalized === 'ag_ui' || normalized === 'ag-ui') return 'ag_ui';
  if (normalized === 'mcp_apps' || normalized === 'mcp-apps') return 'mcp_apps';
  return null;
}

function protocolInstruction(protocol: UiProtocol): string {
  if (protocol === 'mcp_apps') {
    return [
      'Use adk-ui tools and explicitly set `protocol` to `mcp_apps`.',
      'Include `mcp_apps.domain` as `https://example.com` when you call render tools.',
      'Prefer rich components (cards, tables, charts, alerts, forms) instead of plain text-only layouts.',
      'Prefer `render_layout`, `render_table`, `render_chart`, and `render_form` when they fit the request.',
      'Call at least one `render_*` tool before any plain text response.',
      'If rendering fails, call `render_alert` with an error message.',
      'Return rich UI with forms, charts, and actionable controls.',
    ].join(' ');
  }
  return [
    `Use adk-ui tools and explicitly set \`protocol\` to \`${protocol}\`.`,
    'Prefer rich components (cards, tables, charts, alerts, forms) instead of plain text-only layouts.',
    'Prefer `render_layout`, `render_table`, `render_chart`, and `render_form` when they fit the request.',
    'Call at least one `render_*` tool before any plain text response.',
    'If rendering fails, call `render_alert` with an error message.',
    'Return rich UI with layouts, data visuals, and clear actions.',
  ].join(' ');
}

function retryInstruction(
  protocol: UiProtocol,
  mode: 'no_surface' | 'quality' = 'no_surface',
): string {
  if (mode === 'quality') {
    if (protocol === 'mcp_apps') {
      return [
        'Retry mode: previous attempt produced a low-fidelity UI (mostly text/buttons).',
        'Now produce a richer surface using render tools: include at least one table, one card, and one alert.',
        'Use render_layout for composition and avoid plain text-only timelines/lists.',
        'Do not emit plain text before tool calls.',
      ].join(' ');
    }

    return [
      'Retry mode: previous attempt produced a low-fidelity UI (mostly text/buttons).',
      `Now produce a richer \`${protocol}\` surface using render tools: include at least one table, one card, and one alert.`,
      'Use render_layout for composition and avoid plain text-only timelines/lists.',
      'Do not emit plain text before tool calls.',
    ].join(' ');
  }

  if (protocol === 'mcp_apps') {
    return [
      'Retry mode: previous attempt produced no renderable UI.',
      'Now emit exactly one valid `render_screen` tool call immediately with `protocol: "mcp_apps"` and a `root` component.',
      'Ensure the surface includes structured UI (table/card/alert), not only text and buttons.',
      'Do not emit plain text before the tool call.',
    ].join(' ');
  }
  return [
    'Retry mode: previous attempt produced no renderable UI.',
    `Now emit exactly one valid \`render_screen\` tool call immediately with \`protocol: "${protocol}"\` and a \`root\` component.`,
    'Ensure the surface includes structured UI (table/card/alert), not only text and buttons.',
    'Do not emit plain text before the tool call.',
  ].join(' ');
}

function toRawComponents(value: unknown): unknown[] | null {
  if (Array.isArray(value)) {
    return value;
  }

  if (isRecord(value)) {
    // Single component object shape.
    if ('component' in value || 'type' in value) {
      return [value];
    }

    // Component map shape: { "id-1": {...}, "id-2": {...} }
    const mapped = Object.entries(value).map(([id, entry]) => {
      if (isRecord(entry)) {
        if (typeof pickField(entry, 'id') === 'string') {
          return entry;
        }
        return { id, ...entry };
      }
      return entry;
    });
    return mapped.length > 0 ? mapped : null;
  }

  if (typeof value === 'string') {
    try {
      const parsed = JSON.parse(value);
      return toRawComponents(parsed);
    } catch {
      return null;
    }
  }
  return null;
}

function extractTextValue(value: unknown): string {
  if (typeof value === 'string') {
    return value;
  }
  if (!isRecord(value)) {
    return '';
  }
  const literal = pickField(value, 'literalString', 'literal_string');
  if (typeof literal === 'string') {
    return literal;
  }
  const dynamic = pickField(value, 'dynamicString', 'dynamic_string');
  if (typeof dynamic === 'string') {
    return dynamic;
  }
  const text = pickField(value, 'text');
  return typeof text === 'string' ? text : '';
}

function extractLegacyUiResponseComponents(dataModel: unknown): Component[] | null {
  if (!isRecord(dataModel)) {
    return null;
  }

  const legacy = pickField(dataModel, 'adk_ui_response', 'adkUiResponse');
  if (!isRecord(legacy)) {
    return null;
  }

  const rawComponents = pickField(legacy, 'components');
  if (!Array.isArray(rawComponents)) {
    return null;
  }

  const components = rawComponents
    .filter((entry): entry is Record<string, unknown> => isRecord(entry))
    .filter((entry) => typeof pickField(entry, 'type') === 'string')
    .map((entry) => entry as unknown as Component);

  return components.length > 0 ? components : null;
}

function parseTableColumnDef(entry: Record<string, unknown>): TableColumnDef | null {
  const componentField = pickField(entry, 'component');
  let tableColumnNode: Record<string, unknown> | null = null;

  if (isRecord(componentField)) {
    const candidate = pickField(componentField, 'TableColumn', 'tableColumn', 'table_column', 'column');
    if (isRecord(candidate)) {
      tableColumnNode = candidate;
    } else {
      const maybeAccessor = pickField(componentField, 'accessorKey', 'accessor_key', 'accessor');
      if (typeof maybeAccessor === 'string') {
        tableColumnNode = componentField;
      }
    }
  } else if (typeof componentField === 'string') {
    const normalized = componentField.toLowerCase();
    if (normalized === 'tablecolumn' || normalized === 'table_column' || normalized === 'table-column') {
      tableColumnNode = entry;
    }
  }

  if (!tableColumnNode) {
    const direct = pickField(entry, 'TableColumn', 'tableColumn', 'table_column');
    if (isRecord(direct)) {
      tableColumnNode = direct;
    }
  }

  if (!tableColumnNode) {
    return null;
  }

  const accessor = pickField(tableColumnNode, 'accessorKey', 'accessor_key', 'accessor');
  if (typeof accessor !== 'string' || accessor.trim().length === 0) {
    return null;
  }

  const headerRaw = pickField(tableColumnNode, 'header', 'title', 'label');
  const header = extractTextValue(headerRaw) || (typeof headerRaw === 'string' ? headerRaw : '') || humanizeId(accessor);
  const sortableRaw = pickField(tableColumnNode, 'sortable');
  const sortable = typeof sortableRaw === 'boolean' ? sortableRaw : undefined;

  return {
    header,
    accessor_key: accessor,
    sortable,
  };
}

function extractTableColumnDefs(
  rawComponents: unknown[],
): Map<string, TableColumnDef> {
  const refs = new Map<string, TableColumnDef>();

  for (const entry of rawComponents) {
    if (!isRecord(entry)) {
      continue;
    }

    const columnId = typeof pickField(entry, 'id') === 'string' ? (pickField(entry, 'id') as string) : null;
    if (!columnId) {
      continue;
    }

    const def = parseTableColumnDef(entry);
    if (!def) {
      continue;
    }

    refs.set(columnId, def);
  }

  return refs;
}

function findTableColumnDef(rawComponents: unknown[], columnRef: string): TableColumnDef | null {
  for (const entry of rawComponents) {
    if (!isRecord(entry)) {
      continue;
    }
    const id = pickField(entry, 'id');
    if (id !== columnRef) {
      continue;
    }
    const parsed = parseTableColumnDef(entry);
    if (parsed) {
      return parsed;
    }
  }
  return null;
}

function inferAccessorFromRef(columnRef: string, sampleKeys: string[], columnIndex: number): string | null {
  if (sampleKeys.length === 0) {
    return null;
  }

  const lowered = columnRef.toLowerCase();
  const normalized = lowered.replace(/^col[-_]/, '').replace(/^column[-_]/, '');
  const compact = normalized.replace(/[-_]/g, '');

  const direct = sampleKeys.find((key) => key.toLowerCase() === lowered);
  if (direct) {
    return direct;
  }

  const normalizedMatch = sampleKeys.find((key) => key.toLowerCase() === normalized);
  if (normalizedMatch) {
    return normalizedMatch;
  }

  const compactMatch = sampleKeys.find((key) => key.toLowerCase().replace(/[-_]/g, '') === compact);
  if (compactMatch) {
    return compactMatch;
  }

  if (columnIndex >= 0 && columnIndex < sampleKeys.length) {
    return sampleKeys[columnIndex];
  }

  return null;
}

function inferTableColumnDef(
  columnRef: string,
  sampleRow: Record<string, unknown> | null,
  columnIndex: number,
): TableColumnDef | null {
  if (!sampleRow) {
    return null;
  }
  const keys = Object.keys(sampleRow);
  const accessor = inferAccessorFromRef(columnRef, keys, columnIndex);
  if (!accessor) {
    return null;
  }
  return {
    header: humanizeId(accessor),
    accessor_key: accessor,
  };
}

function normalizeRequestError(error: unknown, baseUrl: string, endpoint: string, port: number): string {
  const fallback = `Request to ${baseUrl}${endpoint} failed.`;
  if (!(error instanceof Error)) {
    return fallback;
  }

  const message = error.message || fallback;
  const networkPattern =
    /failed to fetch|load failed|networkerror|fetch failed|could not connect|connection refused|request blocked/i;
  if (networkPattern.test(message)) {
    return `Cannot reach ${baseUrl}${endpoint}. Start the example server on port ${port} and retry.`;
  }

  return message;
}

function extractButtonChildRefs(rawComponents: unknown[]): Map<string, string> {
  const refs = new Map<string, string>();

  for (const entry of rawComponents) {
    if (!isRecord(entry)) {
      continue;
    }

    const buttonId = typeof pickField(entry, 'id') === 'string' ? (pickField(entry, 'id') as string) : null;
    if (!buttonId) {
      continue;
    }

    const componentField = pickField(entry, 'component');
    if (isRecord(componentField)) {
      const buttonNode = pickField(componentField, 'Button');
      if (isRecord(buttonNode)) {
        const childRef = pickField(buttonNode, 'child');
        if (typeof childRef === 'string' && childRef.trim().length > 0) {
          refs.set(buttonId, childRef);
        }
      }
      continue;
    }

    if (typeof componentField === 'string' && componentField === 'Button') {
      const childRef = pickField(entry, 'child');
      if (typeof childRef === 'string' && childRef.trim().length > 0) {
        refs.set(buttonId, childRef);
      }
    }
  }

  return refs;
}

function humanizeId(id: string): string {
  const text = id
    .replace(/[_-]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  if (text.length === 0) {
    return 'Submit';
  }
  return text.replace(/\b\w/g, (char) => char.toUpperCase());
}

function convertRawComponent(raw: unknown, fallbackId: string): { id?: string; component: Component } | null {
  if (!isRecord(raw)) {
    return null;
  }

  if (typeof raw.type === 'string') {
    const component = raw as unknown as Component;
    return {
      id: typeof raw.id === 'string' ? raw.id : undefined,
      component,
    };
  }

  const componentField = pickField(raw, 'component');

  let componentObject: Record<string, unknown> | null = null;
  if (isRecord(componentField)) {
    componentObject = componentField;
  } else if (typeof componentField === 'string') {
    // Support adk-ui flat payload shape: { component: "Text", text: "...", ... }
    const inlineProps: Record<string, unknown> = {};
    for (const [key, value] of Object.entries(raw)) {
      if (key === 'id' || key === 'component') {
        continue;
      }
      inlineProps[key] = value;
    }
    componentObject = { [componentField]: inlineProps };
  }

  if (!componentObject) {
    return null;
  }

  const sourceId =
    typeof pickField(raw, 'id') === 'string' ? (pickField(raw, 'id') as string) : fallbackId;
  const converted = convertA2UIComponent({
    id: sourceId,
    component: componentObject,
  });

  if (!converted) {
    return null;
  }

  return {
    id: sourceId,
    component: converted,
  };
}

function resolveNestedComponent(
  component: Component,
  byId: Map<string, Component>,
  path: Set<string>,
): Component {
  const clone = { ...(component as Record<string, unknown>) } as Record<string, unknown>;
  const currentId = typeof clone.id === 'string' ? clone.id : undefined;

  if (currentId) {
    if (path.has(currentId)) {
      return component;
    }
    path.add(currentId);
  }

  if (Array.isArray(clone.children)) {
    clone.children = clone.children
      .map((entry) => {
        if (typeof entry === 'string') {
          return byId.get(entry);
        }
        return entry;
      })
      .filter((entry): entry is Component => Boolean(entry))
      .map((entry) => resolveNestedComponent(entry, byId, new Set(path)));
  }

  if (Array.isArray(clone.content)) {
    clone.content = clone.content
      .map((entry) => {
        if (typeof entry === 'string') {
          return byId.get(entry);
        }
        return entry;
      })
      .filter((entry): entry is Component => Boolean(entry) && isRecord(entry))
      .map((entry) => resolveNestedComponent(entry, byId, new Set(path)));
  }

  if (Array.isArray(clone.footer)) {
    clone.footer = clone.footer
      .map((entry) => {
        if (typeof entry === 'string') {
          return byId.get(entry);
        }
        return entry;
      })
      .filter((entry): entry is Component => Boolean(entry) && isRecord(entry))
      .map((entry) => resolveNestedComponent(entry, byId, new Set(path)));
  }

  if (Array.isArray(clone.tabs)) {
    clone.tabs = clone.tabs
      .filter((entry) => isRecord(entry) && Array.isArray(entry.content))
      .map((tab) => ({
        ...tab,
        content: (tab.content as unknown[])
          .map((entry) => {
            if (typeof entry === 'string') {
              return byId.get(entry);
            }
            return entry;
          })
          .filter((entry): entry is Component => Boolean(entry) && isRecord(entry))
          .map((entry) => resolveNestedComponent(entry, byId, new Set(path))),
      }));
  }

  return clone as Component;
}

function buildRenderableComponents(rawComponents: unknown[]): Component[] {
  const byId = new Map<string, Component>();
  const withoutId: Component[] = [];
  const buttonChildRefs = extractButtonChildRefs(rawComponents);
  const tableColumnDefs = extractTableColumnDefs(rawComponents);

  rawComponents.forEach((entry, index) => {
    const converted = convertRawComponent(entry, `component-${index + 1}`);
    if (!converted) {
      return;
    }
    if (converted.id) {
      byId.set(converted.id, converted.component);
    } else {
      withoutId.push(converted.component);
    }
  });

  // A2UI buttons may reference a child Text node by id. Hydrate label from that node.
  for (const [buttonId, childId] of buttonChildRefs) {
    const buttonNode = byId.get(buttonId);
    if (!buttonNode || buttonNode.type !== 'button') {
      continue;
    }
    const childNode = byId.get(childId);
    const textLabel =
      childNode && childNode.type === 'text' && childNode.content.trim().length > 0
        ? childNode.content
        : null;

    const currentLabel = buttonNode.label?.trim() ?? '';
    if (currentLabel.length === 0 || currentLabel === childId) {
      byId.set(buttonId, {
        ...buttonNode,
        label: textLabel ?? humanizeId(childId),
      });
    }
  }

  // A2UI tables can declare columns as ids and define TableColumn components separately.
  for (const [componentId, component] of byId.entries()) {
    if (component.type !== 'table') {
      continue;
    }

    const rawColumns = (component as unknown as { columns?: unknown[] }).columns;
    if (!Array.isArray(rawColumns)) {
      continue;
    }
    const rawData = (component as unknown as { data?: unknown[] }).data;
    const sampleRow =
      Array.isArray(rawData) && rawData.length > 0 && isRecord(rawData[0])
        ? (rawData[0] as Record<string, unknown>)
        : null;

    const hydrated = rawColumns
      .map((column, index) => {
        if (typeof column === 'string') {
          const fromMap = tableColumnDefs.get(column);
          if (fromMap) {
            return fromMap;
          }
          const fromRaw = findTableColumnDef(rawComponents, column);
          if (fromRaw) {
            return fromRaw;
          }
          return inferTableColumnDef(column, sampleRow, index);
        }
        if (isRecord(column)) {
          const accessor = pickField(column, 'accessor_key', 'accessorKey');
          if (typeof accessor !== 'string' || accessor.trim().length === 0) {
            return null;
          }
          const header = extractTextValue(pickField(column, 'header')) || humanizeId(accessor);
          const sortableRaw = pickField(column, 'sortable');
          const sortable = typeof sortableRaw === 'boolean' ? sortableRaw : undefined;
          return {
            header,
            accessor_key: accessor,
            sortable,
          };
        }
        return null;
      })
      .filter(
        (column): column is { header: string; accessor_key: string; sortable?: boolean } =>
          Boolean(column),
      );

    if (hydrated.length > 0) {
      byId.set(componentId, {
        ...component,
        columns: hydrated,
      });
    }
  }

  const referenced = new Set<string>();
  byId.forEach((component) => {
    const node = component as Record<string, unknown>;
    const maybeChildren = node.children;
    const maybeContent = node.content;
    const maybeFooter = node.footer;
    const maybeTabs = node.tabs;

    const collectRefs = (entries: unknown) => {
      if (!Array.isArray(entries)) {
        return;
      }
      for (const entry of entries) {
        if (typeof entry === 'string') {
          referenced.add(entry);
        }
      }
    };

    collectRefs(maybeChildren);
    collectRefs(maybeContent);
    collectRefs(maybeFooter);

    if (Array.isArray(maybeTabs)) {
      for (const tab of maybeTabs) {
        if (!isRecord(tab)) {
          continue;
        }
        collectRefs(tab.content);
      }
    }
  });

  if (byId.has('root')) {
    return [resolveNestedComponent(byId.get('root') as Component, byId, new Set())];
  }

  const rootCandidates = Array.from(byId.entries())
    .filter(([id]) => !referenced.has(id))
    .map(([, component]) => resolveNestedComponent(component, byId, new Set()));

  if (rootCandidates.length > 0) {
    return [...rootCandidates, ...withoutId];
  }

  const fallback = Array.from(byId.values()).map((component) =>
    resolveNestedComponent(component, byId, new Set()),
  );

  return [...fallback, ...withoutId];
}

function extractSurfaceFromRaw(raw: Record<string, unknown>): SurfaceSnapshot | null {
  const rawComponents = toRawComponents(pickField(raw, 'components'));
  if (!rawComponents || rawComponents.length === 0) {
    return null;
  }

  const surfaceId =
    (typeof pickField(raw, 'surfaceId', 'surface_id') === 'string'
      ? (pickField(raw, 'surfaceId', 'surface_id') as string)
      : 'main') || 'main';

  const dataModelRaw = pickField(raw, 'dataModel', 'data_model');
  const dataModel = isRecord(dataModelRaw) ? dataModelRaw : {};
  const legacyComponents = extractLegacyUiResponseComponents(dataModel);
  const components = legacyComponents ?? buildRenderableComponents(rawComponents);
  if (components.length === 0) {
    return null;
  }

  return {
    surfaceId,
    components,
    dataModel,
  };
}

function extractFromA2uiJsonl(jsonl: string): SurfaceSnapshot | null {
  const lines = jsonl
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0);

  let surfaceId = 'main';
  let dataModel: Record<string, unknown> = {};
  let rawComponents: unknown[] = [];

  for (const line of lines) {
    let message: unknown;
    try {
      message = JSON.parse(line);
    } catch {
      continue;
    }

    if (!isRecord(message)) {
      continue;
    }

    const createSurface = pickField(message, 'createSurface', 'create_surface');
    if (isRecord(createSurface)) {
      const sid = pickField(createSurface, 'surfaceId', 'surface_id');
      if (typeof sid === 'string') {
        surfaceId = sid;
      }
    }

    const updateDataModel = pickField(message, 'updateDataModel', 'update_data_model');
    if (isRecord(updateDataModel)) {
      const value = pickField(updateDataModel, 'value');
      if (isRecord(value)) {
        dataModel = value;
      }
    }

    const updateComponents = pickField(message, 'updateComponents', 'update_components');
    if (isRecord(updateComponents)) {
      const sid = pickField(updateComponents, 'surfaceId', 'surface_id');
      if (typeof sid === 'string') {
        surfaceId = sid;
      }
      const components = toRawComponents(pickField(updateComponents, 'components'));
      if (components && components.length > 0) {
        rawComponents = components;
      }
    }
  }

  if (rawComponents.length === 0) {
    return null;
  }

  const legacyComponents = extractLegacyUiResponseComponents(dataModel);
  const components = legacyComponents ?? buildRenderableComponents(rawComponents);
  if (components.length === 0) {
    return null;
  }

  return {
    surfaceId,
    components,
    dataModel,
  };
}

function extractFromAgUiEvents(events: unknown[]): SurfaceSnapshot | null {
  for (const entry of events) {
    if (!isRecord(entry)) {
      continue;
    }
    const type = pickField(entry, 'type');
    const name = pickField(entry, 'name');
    if (type !== 'CUSTOM' || name !== 'adk.ui.surface') {
      continue;
    }
    const value = pickField(entry, 'value');
    if (!isRecord(value)) {
      continue;
    }
    const surface = pickField(value, 'surface');
    if (!isRecord(surface)) {
      continue;
    }
    return extractSurfaceFromRaw(surface);
  }
  return null;
}

function extractFromMcpPayload(payload: Record<string, unknown>): SurfaceSnapshot | null {
  const readResponse = pickField(payload, 'resourceReadResponse', 'resource_read_response');
  if (!isRecord(readResponse)) {
    return null;
  }

  const contents = pickField(readResponse, 'contents');
  if (!Array.isArray(contents) || contents.length === 0 || !isRecord(contents[0])) {
    return null;
  }

  const html = pickField(contents[0], 'text');
  if (typeof html !== 'string') {
    return null;
  }

  const scriptMatch = html.match(/<script[^>]*id=["']adk-ui-surface["'][^>]*>([\s\S]*?)<\/script>/i);
  if (!scriptMatch || !scriptMatch[1]) {
    return null;
  }

  try {
    const parsed = JSON.parse(scriptMatch[1].trim());
    if (isRecord(parsed)) {
      return extractSurfaceFromRaw(parsed);
    }
  } catch {
    return null;
  }

  return null;
}

function extractSurfaceFromToolResponse(response: unknown): SurfaceSnapshot | null {
  if (typeof response === 'string') {
    try {
      const parsed = JSON.parse(response);
      return extractSurfaceFromToolResponse(parsed);
    } catch {
      return null;
    }
  }

  if (!isRecord(response)) {
    return null;
  }

  // Common wrappers from different tool/runtime envelopes.
  const nestedKeys = ['render_screen_response', 'render_page_response', 'response', 'result', 'output', 'payload', 'data'];
  for (const nestedKey of nestedKeys) {
    const nested = pickField(response, nestedKey);
    if (nested !== undefined) {
      const fromNested = extractSurfaceFromToolResponse(parseMaybeJson(nested));
      if (fromNested) {
        return fromNested;
      }
    }
  }

  // Some tools return { "..._response": <payload> } where key varies.
  for (const [key, value] of Object.entries(response)) {
    if (!key.toLowerCase().endsWith('_response')) {
      continue;
    }
    const fromVariant = extractSurfaceFromToolResponse(parseMaybeJson(value));
    if (fromVariant) {
      return fromVariant;
    }
  }

  if (toRawComponents(pickField(response, 'components'))) {
    return extractSurfaceFromRaw(response);
  }

  const protocol = normalizeProtocol(pickField(response, 'protocol'));

  if (protocol === 'a2ui') {
    const jsonl = pickField(response, 'jsonl');
    if (typeof jsonl === 'string') {
      const fromJsonl = extractFromA2uiJsonl(jsonl);
      if (fromJsonl) {
        return fromJsonl;
      }
    }
    return extractSurfaceFromRaw(response);
  }

  if (protocol === 'ag_ui') {
    const events = pickField(response, 'events');
    if (Array.isArray(events)) {
      return extractFromAgUiEvents(events);
    }
    return null;
  }

  if (protocol === 'mcp_apps') {
    const payload = pickField(response, 'payload');
    if (isRecord(payload)) {
      return extractFromMcpPayload(payload);
    }
    return null;
  }

  if (protocol === 'adk_ui') {
    return extractSurfaceFromRaw(response);
  }

  return null;
}

function extractEventText(event: Record<string, unknown>): string {
  const llmResponse = pickField(event, 'llm_response', 'llmResponse');
  const nestedContent = isRecord(llmResponse) ? pickField(llmResponse, 'content') : undefined;
  const content = pickField(event, 'content') ?? nestedContent;
  if (!isRecord(content)) {
    return '';
  }

  const parts = pickField(content, 'parts');
  if (!Array.isArray(parts)) {
    return '';
  }

  const snippets: string[] = [];
  for (const part of parts) {
    if (!isRecord(part)) {
      continue;
    }
    const text = pickField(part, 'text');
    if (typeof text === 'string') {
      snippets.push(text);
    }
  }

  return snippets.join(' ').trim();
}

type ToolResponseSource = 'call' | 'response';

function extractToolResponses(
  event: Record<string, unknown>,
): Array<{ name: string; response: unknown; source: ToolResponseSource }> {
  const llmResponse = pickField(event, 'llm_response', 'llmResponse');
  const nestedContent = isRecord(llmResponse) ? pickField(llmResponse, 'content') : undefined;
  const content = pickField(event, 'content') ?? nestedContent;
  if (!isRecord(content)) {
    return [];
  }

  const parts = pickField(content, 'parts');
  if (!Array.isArray(parts)) {
    return [];
  }

  const responses: Array<{ name: string; response: unknown; source: ToolResponseSource }> = [];

  for (const part of parts) {
    if (!isRecord(part)) {
      continue;
    }

    const functionCallWrapped = pickField(part, 'functionCall', 'function_call', 'toolCall', 'tool_call');
    if (isRecord(functionCallWrapped)) {
      const callName = pickField(functionCallWrapped, 'name', 'toolName', 'tool_name');
      const callArgs = pickField(functionCallWrapped, 'args', 'arguments', 'parameters', 'input', 'payload');
      if (typeof callName === 'string') {
        responses.push({
          name: callName,
          response: parseMaybeJson(callArgs ?? functionCallWrapped),
          source: 'call',
        });
      }
      continue;
    }

    // adk-core FunctionCall part shape is untagged: { name, args, id? }
    const directCallName = pickField(part, 'name');
    const directCallArgs = pickField(part, 'args', 'arguments', 'parameters', 'input', 'payload');
    if (typeof directCallName === 'string' && directCallArgs !== undefined) {
      responses.push({
        name: directCallName,
        response: parseMaybeJson(directCallArgs),
        source: 'call',
      });
      continue;
    }

    const functionResponse = pickField(part, 'functionResponse', 'function_response', 'toolResponse', 'tool_response');
    if (!isRecord(functionResponse)) {
      continue;
    }

    const name =
      (typeof pickField(functionResponse, 'name', 'toolName', 'tool_name') === 'string'
        ? (pickField(functionResponse, 'name', 'toolName', 'tool_name') as string)
        : 'tool_response') || 'tool_response';

    responses.push({
      name,
      response:
        parseMaybeJson(
          pickField(functionResponse, 'response', 'result', 'output', 'payload', 'data') ??
            functionResponse,
        ),
      source: 'response',
    });
  }

  // Also accept event-level tool response envelopes.
  const eventToolResponse = pickField(event, 'tool_response', 'toolResponse');
  if (isRecord(eventToolResponse)) {
    const nameRaw = pickField(eventToolResponse, 'name', 'toolName', 'tool_name');
    const name = typeof nameRaw === 'string' ? nameRaw : 'tool_response';
    responses.push({
      name,
      response:
        parseMaybeJson(
          pickField(eventToolResponse, 'response', 'result', 'output', 'payload', 'data') ??
            eventToolResponse,
        ),
      source: 'response',
    });
  }

  const eventToolResponses = pickField(event, 'tool_responses', 'toolResponses');
  if (Array.isArray(eventToolResponses)) {
    for (const item of eventToolResponses) {
      if (!isRecord(item)) {
        continue;
      }
      const nameRaw = pickField(item, 'name', 'toolName', 'tool_name');
      const name = typeof nameRaw === 'string' ? nameRaw : 'tool_response';
      responses.push({
        name,
        response: parseMaybeJson(pickField(item, 'response', 'result', 'output', 'payload', 'data') ?? item),
        source: 'response',
      });
    }
  }

  return responses;
}

function extractEventError(event: Record<string, unknown>): string | null {
  const candidates = [
    pickField(event, 'error_message', 'errorMessage'),
    isRecord(pickField(event, 'llm_response', 'llmResponse'))
      ? pickField(pickField(event, 'llm_response', 'llmResponse') as Record<string, unknown>, 'error_message', 'errorMessage')
      : undefined,
  ];

  for (const candidate of candidates) {
    if (typeof candidate === 'string' && candidate.trim().length > 0) {
      return candidate.trim();
    }
  }

  return null;
}

function extractArtifactDelta(event: Record<string, unknown>): Array<{ name: string; version: number }> {
  const actions = pickField(event, 'actions');
  if (!isRecord(actions)) {
    return [];
  }

  const delta = pickField(actions, 'artifact_delta', 'artifactDelta');
  if (!isRecord(delta)) {
    return [];
  }

  const entries: Array<{ name: string; version: number }> = [];
  for (const [name, value] of Object.entries(delta)) {
    const numericVersion = typeof value === 'number' ? value : Number(value);
    if (!Number.isFinite(numericVersion)) {
      continue;
    }
    entries.push({ name, version: numericVersion });
  }

  return entries;
}

function toReadablePreview(raw: unknown, max = 140): string {
  let text: string;
  if (typeof raw === 'string') {
    text = raw;
  } else {
    try {
      text = JSON.stringify(raw);
    } catch {
      text = String(raw);
    }
  }
  return text.length > max ? `${text.slice(0, max)}...` : text;
}

function toPrettyJson(raw: unknown): string {
  try {
    return JSON.stringify(raw, null, 2);
  } catch {
    return String(raw);
  }
}

function trimForPanel(text: string, max = 6000): string {
  if (text.length <= max) {
    return text;
  }
  return `${text.slice(0, max)}\n... (truncated)`;
}

function flattenSurfaceComponents(components: Component[]): Component[] {
  const result: Component[] = [];
  const stack: Component[] = [...components];

  while (stack.length > 0) {
    const node = stack.pop();
    if (!node) {
      continue;
    }
    result.push(node);

    const asRecord = node as Record<string, unknown>;
    const children = asRecord.children;
    const content = asRecord.content;
    const footer = asRecord.footer;
    const tabs = asRecord.tabs;

    if (Array.isArray(children)) {
      for (const entry of children) {
        if (isRecord(entry) && typeof pickField(entry as Record<string, unknown>, 'type') === 'string') {
          stack.push(entry as Component);
        }
      }
    }

    if (Array.isArray(content)) {
      for (const entry of content) {
        if (isRecord(entry) && typeof pickField(entry as Record<string, unknown>, 'type') === 'string') {
          stack.push(entry as Component);
        }
      }
    }

    if (Array.isArray(footer)) {
      for (const entry of footer) {
        if (isRecord(entry) && typeof pickField(entry as Record<string, unknown>, 'type') === 'string') {
          stack.push(entry as Component);
        }
      }
    }

    if (Array.isArray(tabs)) {
      for (const tab of tabs) {
        if (!isRecord(tab) || !Array.isArray(tab.content)) {
          continue;
        }
        for (const entry of tab.content) {
          if (isRecord(entry) && typeof pickField(entry as Record<string, unknown>, 'type') === 'string') {
            stack.push(entry as Component);
          }
        }
      }
    }
  }

  return result;
}

function isLowFidelitySurface(surface: SurfaceSnapshot): boolean {
  const flat = flattenSurfaceComponents(surface.components);
  if (flat.length < 5) {
    return false;
  }

  const structuredTypes = new Set([
    'table',
    'chart',
    'card',
    'grid',
    'tabs',
    'modal',
    'alert',
    'key_value',
    'list',
  ]);

  const typeSet = new Set(flat.map((component) => component.type));
  const hasStructured = flat.some((component) => structuredTypes.has(component.type));
  if (hasStructured) {
    return false;
  }

  const textCount = flat.filter((component) => component.type === 'text').length;
  const buttonCount = flat.filter((component) => component.type === 'button').length;
  const textHeavy = textCount / flat.length >= 0.55;
  const lowVariety = typeSet.size <= 3;

  return textHeavy && lowVariety && buttonCount >= 1;
}

function buildAssistantFallback(
  lastRuntimeEvent: Record<string, unknown> | null,
  lastToolResponse: unknown | null,
  renderDetected: boolean,
): string | null {
  if (!lastRuntimeEvent && !lastToolResponse) {
    return null;
  }

  const lines: string[] = [];
  lines.push('No assistant text was emitted for this run.');

  if (!renderDetected) {
    lines.push('No renderable UI surface was detected in streamed tool payloads.');
  }

  if (lastToolResponse) {
    lines.push('');
    lines.push('Last tool payload:');
    lines.push(toPrettyJson(lastToolResponse));
  }

  if (lastRuntimeEvent) {
    lines.push('');
    lines.push('Last runtime event:');
    lines.push(toPrettyJson(lastRuntimeEvent));
  }

  return trimForPanel(lines.join('\n'));
}

function App() {
  const [selectedExample, setSelectedExample] = useState<ExampleTarget>(EXAMPLES[0]);
  const [selectedProtocol, setSelectedProtocol] = useState<UiProtocol>('a2ui');
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [surface, setSurface] = useState<SurfaceSnapshot | null>(null);
  const [promptInput, setPromptInput] = useState('');
  const [assistantText, setAssistantText] = useState('');
  const [eventLog, setEventLog] = useState<StreamLogEvent[]>([]);
  const [isStreaming, setIsStreaming] = useState(false);
  const [isRetrying, setIsRetrying] = useState(false);
  const [statusText, setStatusText] = useState('Idle');
  const [runCount, setRunCount] = useState(0);
  const [toolHitCount, setToolHitCount] = useState(0);
  const [lastLatencyMs, setLastLatencyMs] = useState<number | null>(null);
  const [errorText, setErrorText] = useState<string | null>(null);
  const [capabilities, setCapabilities] = useState<ProtocolCapability[]>([]);
  const [capabilitiesError, setCapabilitiesError] = useState<string | null>(null);

  const selectedProtocolMeta = useMemo(
    () => PROTOCOLS.find((protocol) => protocol.id === selectedProtocol) ?? PROTOCOLS[0],
    [selectedProtocol],
  );

  const capabilityForSelectedProtocol = useMemo(() => {
    return capabilities.find((capability) => normalizeProtocol(capability.protocol) === selectedProtocol);
  }, [capabilities, selectedProtocol]);

  const baseUrl = `http://localhost:${selectedExample.port}`;

  function appendEvent(kind: string, protocol: UiProtocol, payload: unknown, preview?: string) {
    const entry: StreamLogEvent = {
      id: Date.now() + Math.floor(Math.random() * 1000),
      at: nowLabel(),
      protocol,
      kind,
      preview: preview ?? toReadablePreview(payload),
      raw: payload,
    };

    setEventLog((current) => [entry, ...current].slice(0, MAX_EVENT_LOG));
  }

  function registerUiAction(action: UiEvent) {
    appendEvent('ui_action', selectedProtocol, action, uiEventToMessage(action));
  }

  async function loadCapabilities() {
    const endpoint = '/api/ui/capabilities';
    try {
      const response = await fetch(`${baseUrl}${endpoint}`);
      if (!response.ok) {
        throw new Error(`status ${response.status}${response.statusText ? ` ${response.statusText}` : ''}`);
      }
      const data = await response.json();
      const list = Array.isArray(data.protocols) ? (data.protocols as ProtocolCapability[]) : [];
      setCapabilities(list);
      setCapabilitiesError(null);
      appendEvent('capabilities', selectedProtocol, data, 'Loaded /api/ui/capabilities');
    } catch (error) {
      const message = normalizeRequestError(error, baseUrl, endpoint, selectedExample.port);
      setCapabilities([]);
      setCapabilitiesError(message);
    }
  }

  async function runPrompt(
    rawPrompt: string,
    options?: {
      retryAttempt?: number;
      activeSessionId?: string;
      retryMode?: 'no_surface' | 'quality';
    },
  ) {
    const retryAttempt = options?.retryAttempt ?? 0;
    const isAutoRetry = retryAttempt > 0;
    const prompt = rawPrompt.trim();
    if (!prompt || (isStreaming && !isAutoRetry)) {
      return;
    }

    const activeSessionId = options?.activeSessionId ?? sessionId ?? makeSessionId();
    if (!sessionId && !options?.activeSessionId) {
      setSessionId(activeSessionId);
    }

    setIsStreaming(true);
    setStatusText(isAutoRetry ? 'Streaming UI (retrying)...' : 'Streaming...');
    if (!isAutoRetry) {
      setIsRetrying(false);
    }
    setErrorText(null);
    if (!isAutoRetry) {
      setAssistantText('');
      setEventLog([]);
      setSurface(null);
      setToolHitCount(0);
    }

    const instructedPrompt = `${prompt}\n\n${protocolInstruction(selectedProtocol)}${
      isAutoRetry ? `\n\n${retryInstruction(selectedProtocol, options?.retryMode ?? 'no_surface')}` : ''
    }`;
    const fetchedArtifactVersions = new Set<string>();
    let assistantTextCaptured = false;
    let renderDetected = false;
    let lowFidelityDetected = false;
    let latestSurface: SurfaceSnapshot | null = null;
    let lastRuntimeEventSeen: Record<string, unknown> | null = null;
    let lastToolResponseSeen: unknown | null = null;

    const startedAt = performance.now();
    const endpoint = '/api/run_sse';

    try {
      const response = await fetch(`${baseUrl}${endpoint}`, {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
        },
        body: JSON.stringify({
          appName: selectedExample.id,
          userId: 'user1',
          sessionId: activeSessionId,
          uiProtocol: selectedProtocol,
          streaming: true,
          newMessage: {
            role: 'user',
            parts: [{ text: instructedPrompt }],
          },
        }),
      });

      if (!response.ok || !response.body) {
        throw new Error(
          `request failed (${response.status}${response.statusText ? ` ${response.statusText}` : ''})`,
        );
      }

      appendEvent('request', selectedProtocol, {
        appName: selectedExample.id,
        sessionId: activeSessionId,
        protocol: selectedProtocol,
        prompt,
      });

      const decoder = new TextDecoder();
      const reader = response.body.getReader();
      let buffer = '';
      let shouldStopStreaming = false;

      while (true) {
        const { done, value } = await reader.read();
        if (done) {
          break;
        }

        buffer += decoder.decode(value, { stream: true });

        while (true) {
          const nextBreak = buffer.indexOf('\n');
          if (nextBreak < 0) {
            break;
          }

          const rawLine = buffer.slice(0, nextBreak).trimEnd();
          buffer = buffer.slice(nextBreak + 1);

          if (!rawLine.startsWith('data: ')) {
            continue;
          }

          const payload = rawLine.slice(6).trim();
          if (!payload || payload === ':keep-alive') {
            continue;
          }
          if (payload === '[DONE]') {
            shouldStopStreaming = true;
            break;
          }

          let parsed: unknown;
          try {
            parsed = JSON.parse(payload);
          } catch {
            appendEvent('parse_error', selectedProtocol, payload, 'Could not parse SSE JSON payload');
            continue;
          }

          let runtimeProtocol = selectedProtocol;
          let runtimeEvent = parsed;

          if (isRecord(parsed) && isRecord(parsed.event)) {
            runtimeProtocol = normalizeProtocol(parsed.ui_protocol) ?? selectedProtocol;
            runtimeEvent = parsed.event;
          }

          if (!isRecord(runtimeEvent)) {
            continue;
          }

          lastRuntimeEventSeen = runtimeEvent;
          const runtimeError = extractEventError(runtimeEvent);
          const text = extractEventText(runtimeEvent);
          const toolResponses = extractToolResponses(runtimeEvent);
          const artifactDeltas = extractArtifactDelta(runtimeEvent);
          const turnCompleteRaw = pickField(runtimeEvent, 'turn_complete', 'turnComplete');
          const isTerminalEvent =
            turnCompleteRaw === true ||
            typeof runtimeError === 'string';

          if (isTerminalEvent && !runtimeError && !text && toolResponses.length === 0 && artifactDeltas.length === 0) {
            appendEvent('turn_complete', runtimeProtocol, runtimeEvent, 'Turn complete');
          }

          if (runtimeError) {
            assistantTextCaptured = true;
            setErrorText(runtimeError);
            setAssistantText((current) =>
              trimForPanel(
                `${current}${current ? '\n\n' : ''}[Runtime error]\n${runtimeError}`,
              ),
            );
            appendEvent('runtime_error', runtimeProtocol, runtimeEvent, runtimeError);
          }

          if (text) {
            assistantTextCaptured = true;
            setAssistantText((current) => `${current}${current ? '\n' : ''}${text}`);
          }

          const responseToolResponses = toolResponses.filter((entry) => entry.source === 'response');
          if (responseToolResponses.length > 0) {
            setToolHitCount((count) => count + responseToolResponses.length);
          }

          for (const toolResponse of toolResponses) {
            if (toolResponse.source === 'call') {
              appendEvent(`tool_call:${toolResponse.name}`, runtimeProtocol, toolResponse.response);
              continue;
            }

            lastToolResponseSeen = toolResponse.response;
            appendEvent(`tool:${toolResponse.name}`, runtimeProtocol, toolResponse.response);
            const maybeSurface = extractSurfaceFromToolResponse(toolResponse.response);
            if (maybeSurface) {
              renderDetected = true;
              latestSurface = maybeSurface;
              lowFidelityDetected = isLowFidelitySurface(maybeSurface);
              setSurface(maybeSurface);
              setStatusText(`Rendered ${maybeSurface.surfaceId}`);
            }
          }

          for (const artifactDelta of artifactDeltas) {
            const cacheKey = `${artifactDelta.name}@${artifactDelta.version}`;
            if (fetchedArtifactVersions.has(cacheKey)) {
              continue;
            }
            fetchedArtifactVersions.add(cacheKey);

            try {
              const artifactResponse = await fetch(
                `${baseUrl}/api/sessions/${selectedExample.id}/user1/${activeSessionId}/artifacts/${encodeURIComponent(artifactDelta.name)}`,
              );

              if (!artifactResponse.ok) {
                appendEvent(
                  `artifact:${artifactDelta.name}`,
                  runtimeProtocol,
                  { status: artifactResponse.status, version: artifactDelta.version },
                  `artifact fetch failed (${artifactResponse.status})`,
                );
                continue;
              }

              const artifactBody = await artifactResponse.text();
              appendEvent(
                `artifact:${artifactDelta.name}`,
                runtimeProtocol,
                artifactBody,
                `artifact version ${artifactDelta.version}`,
              );

              let rendered = false;
              const fromJsonl = extractFromA2uiJsonl(artifactBody);
              if (fromJsonl) {
                renderDetected = true;
                latestSurface = fromJsonl;
                lowFidelityDetected = isLowFidelitySurface(fromJsonl);
                setSurface(fromJsonl);
                setStatusText(`Rendered ${fromJsonl.surfaceId} from artifact`);
                rendered = true;
              }

              if (!rendered) {
                try {
                  const parsedArtifact = JSON.parse(artifactBody);
                  const maybeSurface = extractSurfaceFromToolResponse(parsedArtifact);
                  if (maybeSurface) {
                    renderDetected = true;
                    latestSurface = maybeSurface;
                    lowFidelityDetected = isLowFidelitySurface(maybeSurface);
                    setSurface(maybeSurface);
                    setStatusText(`Rendered ${maybeSurface.surfaceId} from artifact`);
                  }
                } catch {
                  // ignore non-JSON artifacts
                }
              }
            } catch (error) {
              appendEvent(
                `artifact:${artifactDelta.name}`,
                runtimeProtocol,
                { error: error instanceof Error ? error.message : 'artifact fetch failed' },
                'artifact fetch error',
              );
            }
          }

          if (isTerminalEvent) {
            shouldStopStreaming = true;
            break;
          }
        }

        if (shouldStopStreaming) {
          try {
            await reader.cancel();
          } catch {
            // ignore cancellation errors
          }
          break;
        }
      }

      const shouldRetry = !renderDetected && !isAutoRetry;
      if (shouldRetry) {
        setIsRetrying(true);
        setStatusText('Streaming UI (retrying)...');
        appendEvent(
          'retry',
          selectedProtocol,
          { reason: 'No renderable UI surface detected in streamed tool payloads.', attempt: 2 },
          'No renderable UI surface detected. Retrying once...',
        );
        return await runPrompt(rawPrompt, { retryAttempt: 1, activeSessionId, retryMode: 'no_surface' });
      }

      const shouldRetryQuality = renderDetected && lowFidelityDetected && !isAutoRetry;
      if (shouldRetryQuality) {
        setIsRetrying(true);
        setStatusText('Streaming UI (retrying quality)...');
        appendEvent(
          'retry',
          selectedProtocol,
          {
            reason: 'Low-fidelity UI detected (mostly text/buttons without structured components).',
            attempt: 2,
            surfaceId: latestSurface?.surfaceId ?? 'main',
          },
          'Low-fidelity UI detected. Retrying once with stricter structure constraints...',
        );
        return await runPrompt(rawPrompt, { retryAttempt: 1, activeSessionId, retryMode: 'quality' });
      }

      if (!assistantTextCaptured) {
        const fallback = buildAssistantFallback(
          lastRuntimeEventSeen,
          lastToolResponseSeen,
          renderDetected,
        );
        if (fallback) {
          setAssistantText(fallback);
        }
      }

      setRunCount((value) => value + 1);
      setStatusText('Completed');
      setLastLatencyMs(Math.round(performance.now() - startedAt));
    } catch (error) {
      const message = normalizeRequestError(error, baseUrl, endpoint, selectedExample.port);
      setStatusText('Failed');
      setErrorText(message);
      appendEvent('error', selectedProtocol, { message }, message);
    } finally {
      setIsStreaming(false);
      if (!isAutoRetry) {
        setIsRetrying(false);
      }
    }
  }

  useEffect(() => {
    setSessionId(null);
    setSurface(null);
    setAssistantText('');
    setErrorText(null);
    setIsRetrying(false);
    setStatusText('Idle');
    setRunCount(0);
    setToolHitCount(0);
    setLastLatencyMs(null);
    setEventLog([]);
    void loadCapabilities();
  }, [baseUrl]);

  return (
    <div className="showcase-shell">
      <div className="ambient-orb ambient-orb-a" />
      <div className="ambient-orb ambient-orb-b" />
      <div className="ambient-grid" />

      <header className="hero-card fade-in-rise">
        <div>
          <p className="eyebrow">ADK-RUST UI EXAMPLES</p>
          <h1>ADK UI</h1>
          <p className="hero-subtitle">
            Create dynamic user interfaces with ADK UI, with support for standard protocols such
            as <strong>A2UI</strong>, <strong>AG-UI</strong>, and <strong>MCP Apps</strong>.
          </p>
        </div>

        <div className="hero-metrics">
          <div className="metric-card">
            <span>Runs</span>
            <strong>{runCount}</strong>
          </div>
          <div className="metric-card">
            <span>Tool Responses</span>
            <strong>{toolHitCount}</strong>
          </div>
          <div className="metric-card">
            <span>Last Latency</span>
            <strong>{lastLatencyMs ? `${lastLatencyMs}ms` : 'n/a'}</strong>
          </div>
          <div className="metric-card">
            <span>Status</span>
            <strong className={isStreaming ? 'status-live' : ''}>{statusText}</strong>
          </div>
        </div>
      </header>

      <section className="control-row fade-in-rise delay-1">
        <div className="select-group">
          <label htmlFor="example-select">Demo App</label>
          <select
            id="example-select"
            value={selectedExample.id}
            onChange={(event) => {
              const next = EXAMPLES.find((example) => example.id === event.target.value);
              if (next) {
                setSelectedExample(next);
              }
            }}
          >
            {EXAMPLES.map((example) => (
              <option key={example.id} value={example.id}>
                {example.name} ({example.port})
              </option>
            ))}
          </select>
          <p>{selectedExample.description}</p>
        </div>

        <div className="select-group">
          <label htmlFor="protocol-select">Protocol Profile</label>
          <select
            id="protocol-select"
            value={selectedProtocol}
            onChange={(event) => {
              const normalized = normalizeProtocol(event.target.value);
              if (normalized) {
                setSelectedProtocol(normalized);
              }
            }}
          >
            {PROTOCOLS.map((protocol) => (
              <option key={protocol.id} value={protocol.id}>
                {protocol.label}
              </option>
            ))}
          </select>
          <p>{selectedProtocolMeta.hint}</p>
        </div>

        <div className="capability-group">
          <div className="capability-title">
            <span>Server Capability Signal</span>
            <button type="button" onClick={() => void loadCapabilities()}>
              Refresh
            </button>
          </div>
          {capabilityForSelectedProtocol ? (
            <>
              <div className="chip-row">
                {capabilityForSelectedProtocol.versions.map((version) => (
                  <span className="chip" key={version}>
                    {version}
                  </span>
                ))}
              </div>
              <div className="feature-list">
                {capabilityForSelectedProtocol.features.slice(0, 4).map((feature) => (
                  <span key={feature}>{feature}</span>
                ))}
              </div>
              {capabilityForSelectedProtocol.deprecation ? (
                <div className="deprecation-note">
                  Legacy window: announced {capabilityForSelectedProtocol.deprecation.announcedOn}
                  {capabilityForSelectedProtocol.deprecation.sunsetTargetOn
                    ? `, sunset ${capabilityForSelectedProtocol.deprecation.sunsetTargetOn}`
                    : ''}
                </div>
              ) : null}
            </>
          ) : (
            <p className="capability-fallback">
              {capabilitiesError
                ? `Capability endpoint unavailable: ${capabilitiesError}`
                : 'No capability metadata loaded for selected protocol.'}
            </p>
          )}
        </div>
      </section>

      <section className="showcase-grid fade-in-rise delay-2">
        <aside className="panel prompt-panel">
          <div className="panel-header">
            <h2>Prompt Launcher</h2>
            <span>{selectedExample.port}</span>
          </div>

          <div className="prompt-grid">
            {selectedExample.prompts.map((prompt) => (
              <button
                key={prompt}
                type="button"
                className="prompt-button"
                onClick={() => void runPrompt(prompt)}
                disabled={isStreaming}
              >
                {prompt}
              </button>
            ))}
          </div>

          <div className="composer-block">
            <label htmlFor="prompt-input">Custom Prompt</label>
            <textarea
              id="prompt-input"
              value={promptInput}
              onChange={(event) => setPromptInput(event.target.value)}
              placeholder="Describe the UI you want the agent to render..."
              rows={5}
              disabled={isStreaming}
            />
            <button
              type="button"
              className="run-button"
              disabled={isStreaming || promptInput.trim().length === 0}
              onClick={() => {
                const value = promptInput;
                setPromptInput('');
                void runPrompt(value);
              }}
            >
              {isRetrying ? 'Retrying...' : isStreaming ? 'Streaming...' : 'Run Live Prompt'}
            </button>
          </div>
        </aside>

        <main className="panel canvas-panel">
          <div className="panel-header">
            <h2>Live Render Canvas</h2>
            <span>{surface ? surface.surfaceId : 'waiting'}</span>
          </div>

          {surface ? (
            <div className="render-surface" data-surface={surface.surfaceId}>
              {surface.components.map((component, index) => (
                <UiRenderer
                  key={`${component.id ?? 'component'}-${index}`}
                  component={component}
                  onAction={registerUiAction}
                />
              ))}
            </div>
          ) : (
            <div className="empty-state">
              {isStreaming ? (
                <h3 className="streaming-title" aria-live="polite">
                  {isRetrying ? 'Streaming UI (retrying)' : 'Streaming UI'}
                  <span className="streaming-dots" aria-hidden="true">
                    <span>.</span>
                    <span>.</span>
                    <span>.</span>
                  </span>
                </h3>
              ) : (
                <h3>No surface rendered yet</h3>
              )}
              {!isStreaming ? (
                <p>
                  Run a prompt to stream tool output and render a dynamic UI surface for{' '}
                  <strong>{selectedProtocolMeta.label}</strong>.
                </p>
              ) : null}
            </div>
          )}

          <div className="assistant-output">
            <h3>Assistant Text Output</h3>
            <pre>{assistantText || 'No text output captured yet.'}</pre>
          </div>
        </main>

        <aside className="panel stream-panel">
          <div className="panel-header">
            <h2>Runtime Event Stream</h2>
            <span>{eventLog.length}</span>
          </div>

          {errorText ? <div className="error-banner">Error: {errorText}</div> : null}

          <div className="event-list">
            {eventLog.length === 0 ? (
              <div className="event-empty">Events will appear here during streaming.</div>
            ) : (
              eventLog.map((entry) => (
                <details key={entry.id} className="event-item">
                  <summary>
                    <div>
                      <span className="event-kind">{entry.kind}</span>
                      <span className="event-preview">{entry.preview}</span>
                    </div>
                    <div className="event-meta">
                      <span>{entry.protocol}</span>
                      <span>{entry.at}</span>
                    </div>
                  </summary>
                  <pre>{JSON.stringify(entry.raw, null, 2)}</pre>
                </details>
              ))
            )}
          </div>
        </aside>
      </section>
    </div>
  );
}

export default App;
