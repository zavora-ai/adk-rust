export type A2uiComponent = Record<string, unknown> & { id: string; component: string };

export interface SurfaceState {
    components: Map<string, A2uiComponent>;
    dataModel: Record<string, unknown>;
}

export class A2uiStore {
    private surfaces = new Map<string, SurfaceState>();

    getSurface(surfaceId: string): SurfaceState | undefined {
        return this.surfaces.get(surfaceId);
    }

    ensureSurface(surfaceId: string): SurfaceState {
        const existing = this.surfaces.get(surfaceId);
        if (existing) {
            return existing;
        }
        const created: SurfaceState = {
            components: new Map(),
            dataModel: {},
        };
        this.surfaces.set(surfaceId, created);
        return created;
    }

    applyUpdateComponents(surfaceId: string, components: A2uiComponent[]) {
        const surface = this.ensureSurface(surfaceId);
        for (const component of components) {
            if (!component.id) {
                continue;
            }
            surface.components.set(component.id, component);
        }
    }

    removeSurface(surfaceId: string) {
        this.surfaces.delete(surfaceId);
    }

    applyUpdateDataModel(surfaceId: string, path: string | undefined, value: unknown) {
        const surface = this.ensureSurface(surfaceId);
        if (!path || path === "/") {
            surface.dataModel = (value as Record<string, unknown>) ?? {};
            return;
        }

        const tokens = path.split("/").filter(Boolean);
        if (tokens.length === 0) {
            surface.dataModel = (value as Record<string, unknown>) ?? {};
            return;
        }

        // Reject prototype-polluting keys
        const FORBIDDEN_KEYS = new Set(["__proto__", "constructor", "prototype"]);

        let cursor: Record<string, unknown> = surface.dataModel;
        for (let i = 0; i < tokens.length - 1; i += 1) {
            const key = tokens[i];
            if (FORBIDDEN_KEYS.has(key)) {
                return;
            }
            const next = cursor[key];
            if (typeof next === "object" && next !== null) {
                cursor = next as Record<string, unknown>;
            } else {
                const created: Record<string, unknown> = {};
                cursor[key] = created;
                cursor = created;
            }
        }
        const lastKey = tokens[tokens.length - 1];
        if (FORBIDDEN_KEYS.has(lastKey)) {
            return;
        }
        if (typeof value === "undefined") {
            delete cursor[lastKey];
        } else {
            cursor[lastKey] = value as unknown;
        }
    }
}
