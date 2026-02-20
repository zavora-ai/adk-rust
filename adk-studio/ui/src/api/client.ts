import type { Project, ProjectMeta } from '../types/project';

const API_BASE = '/api';

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    headers: { 'Content-Type': 'application/json' },
    ...options,
  });
  if (!res.ok) {
    const err = await res.json().catch(() => ({ error: res.statusText }));
    throw new Error(err.error || 'Request failed');
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export interface GeneratedFile {
  path: string;
  content: string;
}

export interface GeneratedProject {
  files: GeneratedFile[];
}

export interface BuildResponse {
  success: boolean;
  output: string;
  binary_path: string | null;
}

export interface DeployRequest {
  spatialOsUrl?: string;
  register?: boolean;
  openSpatialOs?: boolean;
}

export interface SpatialRegistrationResult {
  attempted: boolean;
  success: boolean;
  message: string;
}

export interface DeployResponse {
  success: boolean;
  manifest: unknown;
  manifestPath: string;
  spatialOsUrl: string;
  registration: SpatialRegistrationResult;
  openUrl: string | null;
}

export interface DetectedKeyEntry {
  name: string;
  status: 'detected' | 'not_set';
  masked: string | null;
}

export interface DetectedKeysResponse {
  keys: DetectedKeyEntry[];
}

export interface ProjectKeyEntry {
  name: string;
  source: 'environment' | 'project' | 'not_set';
  masked: string | null;
}

export interface ProjectKeysResponse {
  keys: ProjectKeyEntry[];
}

export const api = {
  projects: {
    list: () => request<ProjectMeta[]>('/projects'),
    get: (id: string) => request<Project>(`/projects/${id}`),
    create: (name: string, description = '') =>
      request<Project>('/projects', {
        method: 'POST',
        body: JSON.stringify({ name, description }),
      }),
    update: (id: string, project: Project) =>
      request<Project>(`/projects/${id}`, {
        method: 'PUT',
        body: JSON.stringify(project),
      }),
    delete: (id: string) =>
      request<void>(`/projects/${id}`, { method: 'DELETE' }),
    compile: (id: string) =>
      request<GeneratedProject>(`/projects/${id}/compile`),
    build: (id: string) =>
      request<BuildResponse>(`/projects/${id}/build`, { method: 'POST' }),
    deploy: (id: string, payload: DeployRequest = {}) =>
      request<DeployResponse>(`/projects/${id}/deploy`, {
        method: 'POST',
        body: JSON.stringify(payload),
      }),
  },
  settings: {
    getDetectedKeys: () =>
      request<DetectedKeysResponse>('/settings/detected-keys'),
  },
  keys: {
    getProjectKeys: (id: string) =>
      request<ProjectKeysResponse>(`/projects/${id}/keys`),
    saveProjectKeys: (id: string, keys: Record<string, string>) =>
      request<ProjectKeysResponse>(`/projects/${id}/keys`, {
        method: 'POST',
        body: JSON.stringify({ keys }),
      }),
    deleteProjectKey: (id: string, name: string) =>
      request<ProjectKeysResponse>(
        `/projects/${id}/keys/${encodeURIComponent(name)}`,
        { method: 'DELETE' },
      ),
  },
};
