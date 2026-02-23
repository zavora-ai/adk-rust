/**
 * VS Code Project Sync for ADK Studio
 *
 * Handles postMessage events from the VS Code extension host for
 * project navigation:
 * - `openProject` — open a specific project by ID
 * - `closeProject` — return to the project list
 *
 * Posts `projectOpened` responses back to the extension host.
 */

import { useEffect } from 'react';
import { useStore } from '../store';

// ---------------------------------------------------------------------------
// Message types: Extension Host → Studio Frontend
// ---------------------------------------------------------------------------

/** Command to open a project by its ID (filesystem path). */
export interface OpenProjectMessage {
  type: 'openProject';
  projectId: string;
}

/** Command to close the current project and return to the project list. */
export interface CloseProjectMessage {
  type: 'closeProject';
}

/** Union of all recognized project-related messages from the extension host. */
export type VSCodeProjectMessage = OpenProjectMessage | CloseProjectMessage;

// ---------------------------------------------------------------------------
// Response type: Studio Frontend → Extension Host
// ---------------------------------------------------------------------------

/** Confirmation sent back after an `openProject` command is processed. */
export interface ProjectOpenedResponse {
  type: 'projectOpened';
  projectId: string;
  success: boolean;
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/**
 * Type-guard that validates an unknown value is a well-formed
 * `VSCodeProjectMessage`.
 *
 * - Rejects non-objects and null values (Requirement 5.1)
 * - Rejects unrecognized `type` fields (Requirement 5.2)
 * - Rejects `openProject` with missing/empty/non-string `projectId` (Requirement 5.3)
 */
export function isVSCodeProjectMessage(data: unknown): data is VSCodeProjectMessage {
  if (typeof data !== 'object' || data === null) return false;
  const msg = data as Record<string, unknown>;
  if (msg.type === 'openProject') {
    return typeof msg.projectId === 'string' && msg.projectId.length > 0;
  }
  if (msg.type === 'closeProject') {
    return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Response posting
// ---------------------------------------------------------------------------

/**
 * Post a `ProjectOpenedResponse` back to the extension host.
 *
 * Uses `window.parent` when running inside an iframe (VS Code webview),
 * falls back to `window` otherwise.
 */
export function postResponse(response: ProjectOpenedResponse): void {
  const target = window.parent !== window ? window.parent : window;
  target.postMessage(response, '*');
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/**
 * Hook that listens for project-related postMessage events from the
 * VS Code extension host and dispatches them to the Zustand store.
 *
 * - Registers a `message` event listener on mount (Requirement 6.1)
 * - Removes the listener on unmount (Requirement 6.2)
 * - Handles `openProject` messages: validates, calls store, posts response (Requirements 2.1, 4.1, 4.2)
 * - Handles `closeProject` messages: calls `closeProject()` on the store (Requirement 3.1)
 * - Logs warnings for failed project opens (Requirement 2.2)
 */
export function useVSCodeProjectSync(): void {
  const openProject = useStore((s) => s.openProject);
  const closeProject = useStore((s) => s.closeProject);

  useEffect(() => {
    function handleMessage(event: MessageEvent) {
      const data: unknown = event.data;
      if (!isVSCodeProjectMessage(data)) return;

      if (data.type === 'openProject') {
        openProject(data.projectId)
          .then(() => {
            postResponse({ type: 'projectOpened', projectId: data.projectId, success: true });
          })
          .catch((err) => {
            console.warn('[ADK Studio] Failed to open project via postMessage: %s', data.projectId, err);
            postResponse({ type: 'projectOpened', projectId: data.projectId, success: false });
          });
      } else if (data.type === 'closeProject') {
        closeProject();
      }
    }

    window.addEventListener('message', handleMessage);
    return () => window.removeEventListener('message', handleMessage);
  }, [openProject, closeProject]);
}

