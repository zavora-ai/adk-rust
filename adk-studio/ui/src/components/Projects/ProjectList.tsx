import { useState } from 'react';
import { useStore } from '../../store';
import { SettingsModal } from '../Overlays/SettingsModal';
import type { ProjectSettings } from '../../types/project';
import { TEMPLATES } from '../MenuBar/templates';

export function ProjectList() {
  const { projects, loadingProjects, createProject, openProject, deleteProject, addAgent, addActionNode, addEdge } = useStore();
  const [showCreate, setShowCreate] = useState(false);
  const [newName, setNewName] = useState('');
  const [settingsProjectId, setSettingsProjectId] = useState<string | null>(null);
  const [settingsProject, setSettingsProject] = useState<{ name: string; description: string; settings: ProjectSettings } | null>(null);
  const [showGlobalSettings, setShowGlobalSettings] = useState(false);

  const handleCreate = async () => {
    if (!newName.trim()) return;
    const project = await createProject(newName.trim());
    setNewName('');
    setShowCreate(false);
    await openProject(project.id);
    
    // Apply default template (simple_chat with trigger)
    const defaultTemplate = TEMPLATES.find(t => t.id === 'simple_chat');
    if (defaultTemplate) {
      // Add action nodes (including trigger)
      if (defaultTemplate.actionNodes) {
        Object.entries(defaultTemplate.actionNodes).forEach(([id, node]) => {
          addActionNode(id, node);
        });
      }
      // Add agents
      Object.entries(defaultTemplate.agents).forEach(([id, agent]) => {
        addAgent(id, agent);
      });
      // Add edges
      defaultTemplate.edges.forEach(e => addEdge(e.from, e.to));
      
      // Save project to persist layout direction
      const { saveProject } = useStore.getState();
      saveProject();
    }
  };

  const handleOpenSettings = async (e: React.MouseEvent, projectId: string) => {
    e.stopPropagation();
    // Fetch full project to get settings
    const { api } = await import('../../api/client');
    const project = await api.projects.get(projectId);
    setSettingsProjectId(projectId);
    setSettingsProject({
      name: project.name,
      description: project.description,
      settings: project.settings,
    });
  };

  const handleSaveSettings = async (settings: ProjectSettings, name: string, description: string) => {
    if (!settingsProjectId) return;
    const { api } = await import('../../api/client');
    const project = await api.projects.get(settingsProjectId);
    await api.projects.update(settingsProjectId, {
      ...project,
      name,
      description,
      settings,
    });
    // Refresh project list
    const { fetchProjects } = useStore.getState();
    await fetchProjects();
    setSettingsProjectId(null);
    setSettingsProject(null);
  };

  return (
    <div className="p-8 max-w-4xl mx-auto">
      <div className="flex justify-between items-center mb-8">
        <h2 className="text-2xl font-bold text-theme-primary">Projects</h2>
        <div className="flex gap-2">
          <button
            onClick={() => setShowGlobalSettings(true)}
            className="px-4 py-2 bg-theme-secondary text-theme-primary border border-theme-default rounded hover:border-studio-accent font-medium"
            title="Global Settings"
          >
            🌐 Settings
          </button>
          <button
            onClick={() => setShowCreate(true)}
            className="px-4 py-2 bg-studio-highlight text-white rounded hover:opacity-90 font-medium"
          >
            + New Project
          </button>
        </div>
      </div>

      {showCreate && (
        <div className="mb-6 p-4 bg-theme-card rounded-lg border border-theme-default shadow-md">
          <input
            type="text"
            placeholder="Project name"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleCreate()}
            className="w-full px-3 py-2 bg-theme-secondary border border-theme-default rounded mb-3 text-theme-primary placeholder-theme-muted focus:border-studio-accent focus:outline-none"
            autoFocus
          />
          <div className="flex gap-2">
            <button onClick={handleCreate} className="px-4 py-2 bg-studio-highlight text-white rounded font-medium">Create</button>
            <button onClick={() => setShowCreate(false)} className="px-4 py-2 bg-theme-secondary text-theme-primary border border-theme-default rounded">Cancel</button>
          </div>
        </div>
      )}

      {loadingProjects ? (
        <div className="text-theme-muted">Loading...</div>
      ) : projects.length === 0 ? (
        <div className="text-theme-muted text-center py-12">No projects yet. Create one to get started!</div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {projects.map((p) => (
            <div
              key={p.id}
              className="p-4 bg-theme-card rounded-lg border border-theme-default hover:border-studio-accent cursor-pointer group shadow-sm hover:shadow-md transition-shadow"
              onClick={() => openProject(p.id)}
            >
              <div className="flex items-start justify-between">
                <span className="font-medium text-theme-primary">📁 {p.name}</span>
                <div className="flex gap-1">
                  <button
                    onClick={(e) => handleOpenSettings(e, p.id)}
                    className="opacity-0 group-hover:opacity-100 text-theme-muted hover:text-theme-primary transition-opacity"
                    title="Project Settings"
                  >⚙️</button>
                  <button
                    onClick={(e) => { e.stopPropagation(); if (confirm('Delete?')) deleteProject(p.id); }}
                    className="opacity-0 group-hover:opacity-100 text-theme-muted hover:text-red-500 transition-opacity"
                  >✕</button>
                </div>
              </div>
              {p.description && <p className="text-sm text-theme-secondary mt-2">{p.description}</p>}
              <p className="text-xs text-theme-muted mt-2">{new Date(p.updated_at).toLocaleDateString()}</p>
            </div>
          ))}
        </div>
      )}

      {/* Settings Modal */}
      {settingsProject && (
        <SettingsModal
          settings={settingsProject.settings}
          projectName={settingsProject.name}
          projectDescription={settingsProject.description}
          projectId={settingsProjectId ?? undefined}
          onSave={handleSaveSettings}
          onClose={() => {
            setSettingsProjectId(null);
            setSettingsProject(null);
          }}
        />
      )}

      {/* Global Settings Modal */}
      {showGlobalSettings && (
        <SettingsModal
          onClose={() => setShowGlobalSettings(false)}
        />
      )}
    </div>
  );
}
