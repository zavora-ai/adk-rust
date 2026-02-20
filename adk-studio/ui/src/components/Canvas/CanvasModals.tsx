import type { GeneratedProject } from '../../api/client';
import type { FunctionToolConfig, ToolConfig, ProjectSettings } from '../../types/project';
import { CodeModal, BuildModal, CodeEditorModal, NewProjectModal, SettingsModal } from '../Overlays';

export interface CanvasModalsProps {
  // CodeModal
  compiledCode: GeneratedProject | null;
  onClearCompiledCode: () => void;

  // BuildModal
  buildOutput: { success: boolean; output: string; path: string | null } | null;
  building: boolean;
  isAutobuild: boolean;
  onClearBuildOutput: () => void;

  // CodeEditorModal
  showCodeEditor: boolean;
  fnConfig: FunctionToolConfig | null;
  selectedToolId: string | null;
  onUpdateToolConfig: (toolId: string, config: ToolConfig) => void;
  onCloseCodeEditor: () => void;

  // NewProjectModal
  showNewProjectModal: boolean;
  onNewProjectConfirm: (name: string) => Promise<void>;
  onCloseNewProjectModal: () => void;

  // SettingsModal
  showSettingsModal: boolean;
  projectId?: string;
  projectSettings: ProjectSettings | undefined;
  projectName: string;
  projectDescription: string;
  settingsInitialTab?: 'general' | 'codegen' | 'ui' | 'env';
  showApiKeyBanner?: boolean;
  onSaveSettings: (settings: ProjectSettings, name: string, description: string) => void;
  onCloseSettingsModal: () => void;
}

export function CanvasModals({
  compiledCode,
  onClearCompiledCode,
  buildOutput,
  building,
  isAutobuild,
  onClearBuildOutput,
  showCodeEditor,
  fnConfig,
  selectedToolId,
  onUpdateToolConfig,
  onCloseCodeEditor,
  showNewProjectModal,
  onNewProjectConfirm,
  onCloseNewProjectModal,
  showSettingsModal,
  projectId,
  projectSettings,
  projectName,
  projectDescription,
  settingsInitialTab,
  showApiKeyBanner,
  onSaveSettings,
  onCloseSettingsModal,
}: CanvasModalsProps) {
  return (
    <>
      {compiledCode && (
        <CodeModal code={compiledCode} onClose={onClearCompiledCode} />
      )}
      {buildOutput && (
        <BuildModal
          building={building}
          success={buildOutput.success}
          output={buildOutput.output}
          path={buildOutput.path}
          onClose={onClearBuildOutput}
          isAutobuild={isAutobuild}
        />
      )}
      {showCodeEditor && fnConfig && (
        <CodeEditorModal
          config={fnConfig}
          onUpdate={c => onUpdateToolConfig(selectedToolId!, c)}
          onClose={onCloseCodeEditor}
        />
      )}
      {showNewProjectModal && (
        <NewProjectModal
          onConfirm={onNewProjectConfirm}
          onClose={onCloseNewProjectModal}
        />
      )}
      {showSettingsModal && projectSettings && (
        <SettingsModal
          settings={projectSettings}
          projectName={projectName}
          projectDescription={projectDescription}
          projectId={projectId}
          initialTab={settingsInitialTab}
          showApiKeyBanner={showApiKeyBanner}
          onSave={onSaveSettings}
          onClose={onCloseSettingsModal}
        />
      )}
    </>
  );
}
