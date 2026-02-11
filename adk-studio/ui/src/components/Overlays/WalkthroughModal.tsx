/**
 * WalkthroughModal component for ADK Studio v2.0
 * 
 * Provides a guided onboarding experience for new users.
 * Guides through: create project, add agents, action nodes, connect nodes, run tests.
 * Features an animated progress bar with shimmer effect.
 * 
 * Requirements: 6.5, 6.6
 */

import { useState, useEffect, useCallback, useRef } from 'react';
import { X, ChevronRight, ChevronLeft, Check, Sparkles } from 'lucide-react';

/**
 * Walkthrough step definition
 */
interface WalkthroughStep {
  id: string;
  title: string;
  description: string;
  icon: string;
  tips: string[];
}

/**
 * Action node showcase items for the Action Nodes walkthrough step
 */
const ACTION_NODE_SHOWCASE = [
  { icon: '🎯', label: 'Trigger', color: '#6366F1', desc: 'Entry points' },
  { icon: '🌐', label: 'HTTP', color: '#3B82F6', desc: 'API calls' },
  { icon: '📝', label: 'Set', color: '#8B5CF6', desc: 'Variables' },
  { icon: '⚙️', label: 'Transform', color: '#EC4899', desc: 'Data ops' },
  { icon: '🔀', label: 'Switch', color: '#F59E0B', desc: 'Branching' },
  { icon: '🔄', label: 'Loop', color: '#10B981', desc: 'Iteration' },
  { icon: '🔗', label: 'Merge', color: '#06B6D4', desc: 'Combine' },
  { icon: '⏱️', label: 'Wait', color: '#6B7280', desc: 'Timing' },
  { icon: '💻', label: 'Code', color: '#EF4444', desc: 'Custom logic' },
  { icon: '🗄️', label: 'Database', color: '#14B8A6', desc: 'Storage' },
  { icon: '📧', label: 'Email', color: '#EA580C', desc: 'Messages' },
  { icon: '🔔', label: 'Notification', color: '#22D3EE', desc: 'Alerts' },
  { icon: '📡', label: 'RSS', color: '#F97316', desc: 'Feeds' },
  { icon: '📁', label: 'File', color: '#A855F7', desc: 'File I/O' },
];

/**
 * Walkthrough steps for new users
 * Requirements: 6.6
 */
const WALKTHROUGH_STEPS: WalkthroughStep[] = [
  {
    id: 'welcome',
    title: 'Welcome to ADK Studio!',
    description: 'ADK Studio is a visual builder for creating AI agent workflows. Let\'s walk through the basics to get you started.',
    icon: '👋',
    tips: [
      'Build complex agent systems visually',
      'Test and debug in real-time',
      'Export production-ready Rust code',
    ],
  },
  {
    id: 'create-project',
    title: 'Create a Project',
    description: 'Start by creating a new project. Each project contains your agent workflow and configuration.',
    icon: '📁',
    tips: [
      'Click "File → New Project" in the menu',
      'Give your project a descriptive name',
      'Or select a template to start quickly',
    ],
  },
  {
    id: 'add-agents',
    title: 'Add Agents',
    description: 'Drag agents from the left palette onto the canvas. Each agent type has different capabilities.',
    icon: '🤖',
    tips: [
      'LLM Agent: Basic AI agent with model access',
      'Sequential: Run agents in order',
      'Parallel: Run agents simultaneously',
      'Loop: Iterate until a condition is met',
      'Router: Route to different agents based on input',
    ],
  },
  {
    id: 'action-nodes',
    title: 'Action Nodes',
    description: 'Action Nodes are deterministic, non-LLM building blocks for your workflows. Mix them with AI agents for powerful automations.',
    icon: '⚡',
    tips: [
      'Drag action nodes from the palette alongside agents',
      'HTTP, Database, and File nodes connect to external services',
      'Switch and Loop nodes control workflow logic',
      'Code nodes let you write custom JavaScript/TypeScript',
    ],
  },
  {
    id: 'connect-nodes',
    title: 'Connect Nodes',
    description: 'Connect agents and action nodes by dragging from one node\'s output handle to another\'s input handle.',
    icon: '🔗',
    tips: [
      'Drag from the bottom handle to the top handle',
      'Double-click an edge to remove it',
      'Use the auto-layout button to organize nodes',
    ],
  },
  {
    id: 'configure-agents',
    title: 'Configure Agents',
    description: 'Click on an agent to open its properties panel. Configure the model, instructions, and tools.',
    icon: '⚙️',
    tips: [
      'Set the system instruction for each agent',
      'Add tools like Google Search or Code Execution',
      'Configure model parameters like temperature',
    ],
  },
  {
    id: 'run-tests',
    title: 'Build & Test',
    description: 'Build your project and test it in the console. Watch agents execute in real-time!',
    icon: '▶️',
    tips: [
      'Click "Build" to compile your workflow',
      'Use the console to send test messages',
      'Watch the timeline to debug execution',
      'Inspect state at each node',
    ],
  },
  {
    id: 'complete',
    title: 'You\'re Ready!',
    description: 'You now know the basics of ADK Studio. Explore templates, experiment with different agent types, and build amazing AI workflows!',
    icon: '🎉',
    tips: [
      'Browse the Template Gallery for inspiration',
      'Export your workflow as Rust code',
      'Check the Help menu for keyboard shortcuts',
    ],
  },
];

interface WalkthroughModalProps {
  /** Callback when walkthrough is completed */
  onComplete: () => void;
  /** Callback when walkthrough is skipped */
  onSkip: () => void;
  /** Callback to close the modal */
  onClose: () => void;
}

/** Auto-advance interval in ms */
const AUTO_ADVANCE_MS = 6000;

/**
 * Walkthrough modal for first-run onboarding
 */
export function WalkthroughModal({ onComplete, onSkip, onClose }: WalkthroughModalProps) {
  const [currentStep, setCurrentStep] = useState(0);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const step = WALKTHROUGH_STEPS[currentStep];
  const isFirstStep = currentStep === 0;
  const isLastStep = currentStep === WALKTHROUGH_STEPS.length - 1;

  const clearTimer = useCallback(() => {
    if (timerRef.current) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const startTimer = useCallback(() => {
    clearTimer();
    timerRef.current = setTimeout(() => {
      setCurrentStep((prev) => {
        if (prev < WALKTHROUGH_STEPS.length - 1) return prev + 1;
        return prev; // stay on last step
      });
    }, AUTO_ADVANCE_MS);
  }, [clearTimer]);

  // Restart timer whenever step changes
  useEffect(() => {
    if (!isLastStep) startTimer();
    else clearTimer();
    return clearTimer;
  }, [currentStep, isLastStep, startTimer, clearTimer]);

  const handleNext = () => {
    clearTimer();
    if (isLastStep) {
      onComplete();
    } else {
      setCurrentStep(currentStep + 1);
    }
  };

  const handlePrevious = () => {
    clearTimer();
    if (!isFirstStep) {
      setCurrentStep(currentStep - 1);
    }
  };

  const handleSkip = () => {
    clearTimer();
    onSkip();
  };

  return (
    <div 
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      style={{ backgroundColor: 'rgba(0, 0, 0, 0.6)' }}
      onClick={onClose}
    >
      <div 
        className="w-full max-w-lg rounded-xl shadow-2xl overflow-hidden"
        style={{ backgroundColor: 'var(--surface-panel)' }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div 
          className="flex items-center justify-between px-6 py-4"
          style={{ 
            backgroundColor: 'var(--accent-primary)',
            color: 'white',
          }}
        >
          <div className="flex items-center gap-2">
            <Sparkles size={20} />
            <span className="font-semibold">Getting Started</span>
          </div>
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-white/20 transition-colors"
          >
            <X size={20} />
          </button>
        </div>

        {/* Animated progress bar */}
        <div 
          className="relative h-1.5 overflow-hidden"
          style={{ backgroundColor: 'var(--border-default)' }}
        >
          {/* Filled portion */}
          <div
            className="absolute inset-y-0 left-0 rounded-r-full"
            style={{
              width: `${((currentStep + 1) / WALKTHROUGH_STEPS.length) * 100}%`,
              backgroundColor: 'var(--accent-primary)',
              transition: 'width 0.5s cubic-bezier(0.4, 0, 0.2, 1)',
            }}
          />
          {/* Shimmer overlay on the filled portion */}
          <div
            className="absolute inset-y-0 left-0"
            style={{
              width: `${((currentStep + 1) / WALKTHROUGH_STEPS.length) * 100}%`,
              transition: 'width 0.5s cubic-bezier(0.4, 0, 0.2, 1)',
              background: 'linear-gradient(90deg, transparent 0%, rgba(255,255,255,0.4) 50%, transparent 100%)',
              backgroundSize: '200% 100%',
              animation: 'walkthrough-shimmer 2s ease-in-out infinite',
            }}
          />
        </div>

        {/* Inline keyframes for shimmer animation */}
        <style>{`
          @keyframes walkthrough-shimmer {
            0% { background-position: 200% 0; }
            100% { background-position: -200% 0; }
          }
          .action-node-grid {
            display: flex;
            flex-wrap: wrap;
            gap: 6px;
            justify-content: center;
          }
          .action-node-chip {
            display: flex;
            align-items: center;
            gap: 4px;
            padding: 4px 10px;
            border-radius: 6px;
            font-size: 11px;
            transition: transform 0.15s ease, box-shadow 0.15s ease;
          }
          .action-node-chip:hover {
            transform: translateY(-1px);
            box-shadow: 0 2px 8px rgba(0,0,0,0.12);
          }
        `}</style>

        {/* Content */}
        <div className="px-6 py-6">
          {/* Step icon and title */}
          <div className="text-center mb-6">
            <span className="text-5xl mb-4 block">{step.icon}</span>
            <h2 
              className="text-xl font-bold mb-2"
              style={{ color: 'var(--text-primary)' }}
            >
              {step.title}
            </h2>
            <p 
              className="text-sm"
              style={{ color: 'var(--text-secondary)' }}
            >
              {step.description}
            </p>
          </div>

          {/* Action Nodes showcase grid (only on action-nodes step) */}
          {step.id === 'action-nodes' && (
            <div 
              className="rounded-lg p-3 mb-4"
              style={{ backgroundColor: 'var(--bg-secondary)' }}
            >
              <div className="action-node-grid">
                {ACTION_NODE_SHOWCASE.map((node) => (
                  <div
                    key={node.label}
                    className="action-node-chip"
                    style={{ 
                      backgroundColor: `${node.color}15`,
                      border: `1px solid ${node.color}30`,
                    }}
                  >
                    <span style={{ fontSize: '14px', lineHeight: 1 }}>{node.icon}</span>
                    <span style={{ fontWeight: 600, color: node.color }}>{node.label}</span>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Tips */}
          <div 
            className="rounded-lg p-4 mb-6"
            style={{ backgroundColor: 'var(--bg-secondary)' }}
          >
            <ul className="space-y-2">
              {step.tips.map((tip, index) => (
                <li 
                  key={index}
                  className="flex items-start gap-2 text-sm"
                  style={{ color: 'var(--text-primary)' }}
                >
                  <Check 
                    size={16} 
                    className="mt-0.5 flex-shrink-0"
                    style={{ color: 'var(--accent-primary)' }}
                  />
                  <span>{tip}</span>
                </li>
              ))}
            </ul>
          </div>

          {/* Step counter */}
          <div 
            className="text-center text-xs mb-4"
            style={{ color: 'var(--text-muted)' }}
          >
            Step {currentStep + 1} of {WALKTHROUGH_STEPS.length}
          </div>
        </div>

        {/* Footer with navigation */}
        <div 
          className="flex items-center justify-between px-6 py-4"
          style={{ 
            borderTop: '1px solid var(--border-default)',
            backgroundColor: 'var(--bg-secondary)',
          }}
        >
          <button
            onClick={handleSkip}
            className="px-4 py-2 text-sm rounded transition-colors"
            style={{ color: 'var(--text-secondary)' }}
          >
            Skip Tutorial
          </button>

          <div className="flex items-center gap-2">
            {!isFirstStep && (
              <button
                onClick={handlePrevious}
                className="flex items-center gap-1 px-4 py-2 text-sm rounded transition-colors"
                style={{ 
                  backgroundColor: 'var(--bg-primary)',
                  color: 'var(--text-primary)',
                  border: '1px solid var(--border-default)',
                }}
              >
                <ChevronLeft size={16} />
                Back
              </button>
            )}
            <button
              onClick={handleNext}
              className="flex items-center gap-1 px-4 py-2 text-sm font-medium rounded transition-colors"
              style={{ 
                backgroundColor: 'var(--accent-primary)',
                color: 'white',
              }}
            >
              {isLastStep ? 'Get Started' : 'Next'}
              {!isLastStep && <ChevronRight size={16} />}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
