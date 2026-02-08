import type { ParsedA2uiMessage } from './a2ui/parser';
import { applyParsedMessages, parseJsonl } from './a2ui/parser';
import type { A2uiStore } from './a2ui/store';

type UiSurface = {
    surfaceId: string;
    catalogId: string;
    components: Array<Record<string, unknown>>;
    dataModel?: Record<string, unknown> | null;
    theme?: Record<string, unknown> | null;
    sendDataModel?: boolean;
};

type ProtocolEnvelope = {
    protocol?: string;
    jsonl?: string;
    events?: Array<Record<string, unknown>>;
    payload?: Record<string, unknown>;
};

function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null;
}

function getString(value: unknown): string | undefined {
    return typeof value === 'string' ? value : undefined;
}

function surfaceToJsonl(surface: UiSurface): string {
    const messages: Array<Record<string, unknown>> = [
        {
            createSurface: {
                surfaceId: surface.surfaceId,
                catalogId: surface.catalogId,
                theme: surface.theme ?? undefined,
                sendDataModel: surface.sendDataModel ?? true,
            },
        },
    ];

    if (surface.dataModel) {
        messages.push({
            updateDataModel: {
                surfaceId: surface.surfaceId,
                path: '/',
                value: surface.dataModel,
            },
        });
    }

    messages.push({
        updateComponents: {
            surfaceId: surface.surfaceId,
            components: surface.components,
        },
    });

    return `${messages.map((entry) => JSON.stringify(entry)).join('\n')}\n`;
}

function extractSurfaceFromAgUiEvents(events: Array<Record<string, unknown>>): UiSurface | null {
    for (const event of events) {
        if (getString(event.type) !== 'CUSTOM') {
            continue;
        }
        if (getString(event.name) !== 'adk.ui.surface') {
            continue;
        }
        const value = event.value;
        if (!isRecord(value)) {
            continue;
        }
        const surface = value.surface;
        if (!isRecord(surface)) {
            continue;
        }

        const surfaceId = getString(surface.surfaceId);
        const catalogId = getString(surface.catalogId);
        const components = Array.isArray(surface.components)
            ? surface.components.filter((entry): entry is Record<string, unknown> => isRecord(entry))
            : [];
        if (!surfaceId || !catalogId || components.length === 0) {
            continue;
        }

        return {
            surfaceId,
            catalogId,
            components,
            dataModel: isRecord(surface.dataModel) ? surface.dataModel : undefined,
            theme: isRecord(surface.theme) ? surface.theme : undefined,
            sendDataModel:
                typeof surface.sendDataModel === 'boolean' ? surface.sendDataModel : undefined,
        };
    }

    return null;
}

function extractSurfaceScriptFromHtml(html: string): string | null {
    // Use indexOf-based extraction to avoid polynomial ReDoS with regex on untrusted HTML
    const openTagStart = html.indexOf('<script');
    if (openTagStart === -1) return null;

    const idAttr = html.indexOf('adk-ui-surface', openTagStart);
    if (idAttr === -1) return null;

    const openTagEnd = html.indexOf('>', idAttr);
    if (openTagEnd === -1) return null;

    const closeTag = html.indexOf('</script>', openTagEnd);
    if (closeTag === -1) return null;

    const content = html.substring(openTagEnd + 1, closeTag).trim();
    return content.length > 0 ? content : null;
}

function extractSurfaceFromMcpPayload(payload: Record<string, unknown>): UiSurface | null {
    const resourceReadResponse = payload.resourceReadResponse;
    if (!isRecord(resourceReadResponse)) {
        return null;
    }

    const contents = resourceReadResponse.contents;
    if (!Array.isArray(contents) || contents.length === 0) {
        return null;
    }

    const firstContent = contents[0];
    if (!isRecord(firstContent)) {
        return null;
    }

    const html = getString(firstContent.text);
    if (!html) {
        return null;
    }

    const scriptText = extractSurfaceScriptFromHtml(html);
    if (!scriptText) {
        return null;
    }

    const parsed = JSON.parse(scriptText);
    if (!isRecord(parsed)) {
        return null;
    }

    const surfaceId = getString(parsed.surfaceId);
    const catalogId = getString(parsed.catalogId);
    const components = Array.isArray(parsed.components)
        ? parsed.components.filter((entry): entry is Record<string, unknown> => isRecord(entry))
        : [];
    if (!surfaceId || !catalogId || components.length === 0) {
        return null;
    }

    return {
        surfaceId,
        catalogId,
        components,
        dataModel: isRecord(parsed.dataModel) ? parsed.dataModel : undefined,
        theme: isRecord(parsed.theme) ? parsed.theme : undefined,
        sendDataModel: typeof parsed.sendDataModel === 'boolean' ? parsed.sendDataModel : undefined,
    };
}

function protocolEnvelopeToJsonl(envelope: ProtocolEnvelope): string | null {
    if (typeof envelope.jsonl === 'string') {
        return envelope.jsonl;
    }

    const protocol = getString(envelope.protocol);
    if (protocol === 'ag_ui' && Array.isArray(envelope.events)) {
        const surface = extractSurfaceFromAgUiEvents(
            envelope.events.filter((entry): entry is Record<string, unknown> => isRecord(entry)),
        );
        if (!surface) {
            return null;
        }
        return surfaceToJsonl(surface);
    }

    if (protocol === 'mcp_apps' && isRecord(envelope.payload)) {
        const surface = extractSurfaceFromMcpPayload(envelope.payload);
        if (!surface) {
            return null;
        }
        return surfaceToJsonl(surface);
    }

    return null;
}

export function parseProtocolPayload(payload: unknown): ParsedA2uiMessage[] {
    if (typeof payload === 'string') {
        return parseJsonl(payload);
    }

    if (!isRecord(payload)) {
        return [];
    }

    const maybeEnvelope = payload as ProtocolEnvelope;
    const jsonl = protocolEnvelopeToJsonl(maybeEnvelope);
    if (!jsonl) {
        return [];
    }
    return parseJsonl(jsonl);
}

export function applyProtocolPayload(store: A2uiStore, payload: unknown): ParsedA2uiMessage[] {
    const parsed = parseProtocolPayload(payload);
    if (parsed.length > 0) {
        applyParsedMessages(store, parsed);
    }
    return parsed;
}
