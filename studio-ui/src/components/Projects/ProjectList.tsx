import { useState } from 'react';
import { useStore } from '../../store';

export function ProjectList() {
  const { projects, loadingProjects, createProject, openProject, deleteProject } = useStore();
  const [showCreate, setShowCreate] = useState(false);
  const [newName, setNewName] = useState('');

  const handleCreate = async () => {
    if (!newName.trim()) return;
    const project = await createProject(newName.trim());
    setNewName('');
    setShowCreate(false);
    openProject(project.id);
  };

  return (
    <div className="p-8 max-w-4xl mx-auto">
      <div className="flex justify-between items-center mb-8">
        <h2 className="text-2xl font-bold">Projects</h2>
        <button
          onClick={() => setShowCreate(true)}
          className="px-4 py-2 bg-studio-highlight rounded hover:opacity-90"
        >
          + New Project
        </button>
      </div>

      {showCreate && (
        <div className="mb-6 p-4 bg-studio-panel rounded-lg border border-gray-700">
          <input
            type="text"
            placeholder="Project name"
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            onKeyDown={(e) => e.key === 'Enter' && handleCreate()}
            className="w-full px-3 py-2 bg-studio-bg border border-gray-600 rounded mb-3 text-white"
            autoFocus
          />
          <div className="flex gap-2">
            <button onClick={handleCreate} className="px-4 py-2 bg-studio-highlight rounded">Create</button>
            <button onClick={() => setShowCreate(false)} className="px-4 py-2 bg-gray-700 rounded">Cancel</button>
          </div>
        </div>
      )}

      {loadingProjects ? (
        <div className="text-gray-400">Loading...</div>
      ) : projects.length === 0 ? (
        <div className="text-gray-400 text-center py-12">No projects yet. Create one to get started!</div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-4">
          {projects.map((p) => (
            <div
              key={p.id}
              className="p-4 bg-studio-panel rounded-lg border border-gray-700 hover:border-studio-highlight cursor-pointer group"
              onClick={() => openProject(p.id)}
            >
              <div className="flex items-start justify-between">
                <span className="font-medium">📁 {p.name}</span>
                <button
                  onClick={(e) => { e.stopPropagation(); if (confirm('Delete?')) deleteProject(p.id); }}
                  className="opacity-0 group-hover:opacity-100 text-gray-400 hover:text-red-400"
                >✕</button>
              </div>
              {p.description && <p className="text-sm text-gray-400 mt-2">{p.description}</p>}
              <p className="text-xs text-gray-500 mt-2">{new Date(p.updated_at).toLocaleDateString()}</p>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
