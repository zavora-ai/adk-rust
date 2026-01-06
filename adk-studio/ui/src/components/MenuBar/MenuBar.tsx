import { useState, useRef, useEffect } from 'react';
import { useStore } from '../../store';
import { TEMPLATES, Template } from './templates';

interface MenuBarProps {
  onExportCode: () => void;
  onNewProject: () => void;
  onTemplateApplied?: () => void;
}

export function MenuBar({ onExportCode, onNewProject, onTemplateApplied }: MenuBarProps) {
  const [openMenu, setOpenMenu] = useState<string | null>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const { currentProject, addAgent, removeAgent, addEdge, removeEdge } = useStore();

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
        setOpenMenu(null);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  const applyTemplate = (template: Template) => {
    if (!currentProject) return;

    // Clear existing edges
    (currentProject.workflow?.edges || []).forEach(e => removeEdge(e.from, e.to));

    // Clear existing agents
    Object.keys(currentProject.agents).forEach(id => removeAgent(id));

    // Add all agents from template
    Object.entries(template.agents).forEach(([id, agent]) => {
      addAgent(id, agent);
    });

    // Add edges from template
    template.edges.forEach(e => addEdge(e.from, e.to));

    if (onTemplateApplied) {
      onTemplateApplied();
    }

    setOpenMenu(null);
  };

  const Menu = ({ name, children }: { name: string; children: React.ReactNode }) => (
    <div className="relative">
      <button
        className={`px-3 py-1 text-sm hover:bg-gray-700 rounded ${openMenu === name ? 'bg-gray-700' : ''}`}
        onClick={() => setOpenMenu(openMenu === name ? null : name)}
      >
        {name}
      </button>
      {openMenu === name && (
        <div className="absolute top-full left-0 mt-1 bg-gray-800 border border-gray-600 rounded shadow-lg min-w-[200px] z-50">
          {children}
        </div>
      )}
    </div>
  );

  const MenuItem = ({ onClick, children, disabled }: { onClick: () => void; children: React.ReactNode; disabled?: boolean }) => (
    <button
      className={`w-full text-left px-3 py-2 text-sm hover:bg-gray-700 ${disabled ? 'opacity-50 cursor-not-allowed' : ''}`}
      onClick={() => { if (!disabled) { onClick(); setOpenMenu(null); } }}
      disabled={disabled}
    >
      {children}
    </button>
  );

  const Divider = () => <div className="border-t border-gray-600 my-1" />;

  return (
    <div ref={menuRef} className="flex items-center gap-1 px-2 py-1 bg-gray-900 border-b border-gray-700">
      <span className="text-sm font-semibold text-blue-400 mr-4">🔧 ADK Studio</span>

      <Menu name="File">
        <MenuItem onClick={onNewProject}>📄 New Project</MenuItem>
        <Divider />
        <MenuItem onClick={onExportCode} disabled={!currentProject}>📦 Export Code</MenuItem>
      </Menu>

      <Menu name="Templates">
        <div className="px-3 py-1 text-xs text-gray-400 border-b border-gray-600">Add to current project</div>
        {TEMPLATES.map(t => (
          <MenuItem key={t.id} onClick={() => applyTemplate(t)} disabled={!currentProject}>
            {t.icon} {t.name}
          </MenuItem>
        ))}
      </Menu>

      <Menu name="Help">
        <MenuItem onClick={() => window.open('https://github.com/zavora-ai/adk-rust', '_blank')}>📚 Documentation</MenuItem>
        <Divider />
        <div className="px-3 py-2 text-xs text-gray-400">
          <div className="font-semibold mb-1">Keyboard Shortcuts</div>
          <div>Drag agents from left panel</div>
          <div>Click agent to edit properties</div>
          <div>Drag tools onto agents</div>
        </div>
        <Divider />
        <div className="px-3 py-2 text-xs text-gray-500">ADK Studio v0.1.0</div>
      </Menu>

      <div className="flex-1" />

      {currentProject && (
        <span className="text-sm text-gray-400">
          Project: <span className="text-white">{currentProject.name}</span>
        </span>
      )}
    </div>
  );
}
