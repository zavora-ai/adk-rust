import { useEffect } from 'react';
import { ReactFlowProvider } from '@xyflow/react';
import { useStore } from './store';
import { ProjectList } from './components/Projects/ProjectList';
import { Canvas } from './components/Canvas/Canvas';

export default function App() {
  const { currentProject, fetchProjects } = useStore();

  useEffect(() => {
    fetchProjects();
  }, [fetchProjects]);

  return (
    <div className="h-screen flex flex-col bg-studio-bg">
      <header className="h-12 bg-studio-panel border-b border-gray-700 flex items-center px-4">
        <h1 className="text-lg font-bold text-white flex items-center gap-2">
          <span className="text-2xl">🚀</span> ADK Studio
        </h1>
        {currentProject && (
          <span className="ml-4 text-gray-400">/ {currentProject.name}</span>
        )}
      </header>
      <main className="flex-1 overflow-hidden">
        {currentProject ? (
          <ReactFlowProvider>
            <Canvas />
          </ReactFlowProvider>
        ) : (
          <ProjectList />
        )}
      </main>
    </div>
  );
}
