/**
 * WalkthroughModal component for ADK Studio v2.0
 * 
 * Provides a guided onboarding experience for new users.
 * Features an animated progress bar that fills as each section completes.
 * 
 * Requirements: 6.5, 6.6
 */

import { useState, useEffect, useRef, useCallback } from 'react';
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
  /** Duration in ms the animated bar takes to fill for this step */
  duration: number;
}

/**
 * Walkthrough steps for new users (8 steps including Action Nodes)
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
    duration: 6000,
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
    duration: 5000,
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
    duration: 7000,
  },
  {
    id: 'action-nodes',
    title: 'Action Nodes',
    description: 'Action nodes handle deterministic, non-AI tasks in your workflow — API calls, data transforms, branching, and more.',
    icon: '⚡',
    tips: [
      '🎯 Trigger: Start workflows via webhook, schedule, or event',
      '🌐 HTTP: Make API calls with auth, headers, and body',
      '🔀 Switch: Route data based on conditions',
      '⚙️ Transform: Reshape data with JSONPath or JavaScript',
      '🗄️ Database: Query PostgreSQL, MySQL, MongoDB, Redis',
      '📧 Email & 🔔 Notifications: Send alerts via Slack, Discord, Teams',
      '💻 Code: Run sandboxed JavaScript for custom logic',
    ],
    duration: 8000,
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
    duration: 5000,
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
    duration: 5000,
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
    duration: 5000,
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
    duration: 4000,
  },
];

const TOTAL_STEPS = WALKTHROUGH_STEPS.length;

interface WalkthroughModalProps {
  onComplete: () => void;
  onSkip: () => void;
  onClose: () => void;
}

/**
 * Animated progress bar segment for a single step.
 */
function ProgressSegment({
  index,
  currentStep,
  duration,
  isPaused,
  onSegmentComplete,
}: {
  index: number;
  currentStep: number;
  duration: number;
  isPaused: boolean;
  onSegmentComplete: () => void;
}) {
  const fillRef = useRef<HTMLDivElement>(null);
  const animRef = useRef<Animation | null>(null);

  useEffect(() => {
    const el = fillRef.current;
    if (!el) return;

    if (index < currentStep) {
      // Already completed — fill instantly
      el.style.width = '100%';
      if (animRef.current) { animRef.current.cancel(); animRef.current = null; }
      return;
    }

    if (index > currentStep) {
      // Future step — empty
      el.style.width = '0%';
      if (animRef.current) { animRef.current.cancel(); animRef.current = null; }
      return;
    }

    // Current step — animate fill
    el.style.width = '0%';
    const anim = el.animate(
      [{ width: '0%' }, { width: '100%' }],
      { duration, fill: 'forwards', easing: 'linear' }
    );
    animRef.current = anim;

    anim.onfinish = () => {
      el.style.width = '100%';
      onSegmentComplete();
    };

    return () => { anim.cancel(); };
  }, [index, currentStep, duration, onSegmentComplete]);

  // Pause / resume
  useEffect(() => {
    const anim = animRef.current;
    if (!anim || index !== currentStep) return;
    if (isPaused) {
      anim.pause();
    } else if (anim.playState === 'paused') {
      anim.play();
    }
  }, [isPaused, index, currentStep]);

  return (
    <div
      style={{
        flex: 1,
        height: '3px',
        borderRadius: '2px',
        backgroundColor: 'var(--border-default, #333)',
        overflow: 'hidden',
      }}
    >
      <div
        ref={fillRef}
        style={{
          height: '100%',
          width: '0%',
          borderRadius: '2px',
          background: 'linear-gradient(90deg, var(--accent-primary, #0F8A8A), var(--accent-secondary, #4fd1c5))',
        }}
      />
    </div>
  );
}


/**
 * WalkthroughModal — guided onboarding with animated progress bar.
 *
 * The top bar is split into segments (one per step). The current segment
 * animates from 0→100% over the step's `duration`. When it finishes the
 * modal auto-advances. Users can also click Next/Previous at any time.
 */
export function WalkthroughModal({ onComplete, onSkip, onClose }: WalkthroughModalProps) {
  const [currentStep, setCurrentStep] = useState(0);
  const [isPaused, setIsPaused] = useState(false);
  const step = WALKTHROUGH_STEPS[currentStep];
  const isFirst = currentStep === 0;
  const isLast = currentStep === TOTAL_STEPS - 1;

  const goNext = useCallback(() => {
    if (isLast) {
      onComplete();
    } else {
      setCurrentStep((s) => Math.min(s + 1, TOTAL_STEPS - 1));
    }
  }, [isLast, onComplete]);

  const goPrev = useCallback(() => {
    setCurrentStep((s) => Math.max(s - 1, 0));
  }, []);

  // Keyboard navigation
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'ArrowRight' || e.key === 'Enter') goNext();
      else if (e.key === 'ArrowLeft') goPrev();
      else if (e.key === 'Escape') onClose();
      else if (e.key === ' ') { e.preventDefault(); setIsPaused((p) => !p); }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [goNext, goPrev, onClose]);

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Getting Started Guide"
      style={{
        position: 'fixed', inset: 0, zIndex: 9999,
        display: 'flex', alignItems: 'center', justifyContent: 'center',
        backgroundColor: 'rgba(0,0,0,0.55)',
        backdropFilter: 'blur(4px)',
      }}
      onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
    >
      <div
        style={{
          width: '100%', maxWidth: 560,
          borderRadius: 16, overflow: 'hidden',
          backgroundColor: 'var(--surface-panel, #1e1e2e)',
          border: '1px solid var(--border-default, #333)',
          boxShadow: '0 24px 64px rgba(0,0,0,0.4)',
          color: 'var(--text-primary, #e0e0e0)',
        }}
      >
        {/* ── Animated progress bar ── */}
        <div style={{ display: 'flex', gap: 4, padding: '12px 16px 0 16px' }}>
          {WALKTHROUGH_STEPS.map((s, i) => (
            <ProgressSegment
              key={s.id}
              index={i}
              currentStep={currentStep}
              duration={s.duration}
              isPaused={isPaused}
              onSegmentComplete={goNext}
            />
          ))}
        </div>

        {/* ── Header row ── */}
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', padding: '12px 16px 0' }}>
          <span style={{ fontSize: 12, color: 'var(--text-secondary, #888)' }}>
            {currentStep + 1} / {TOTAL_STEPS}
          </span>
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            {isPaused && (
              <span style={{ fontSize: 11, color: 'var(--text-secondary, #888)', opacity: 0.7 }}>paused</span>
            )}
            <button
              onClick={() => setIsPaused((p) => !p)}
              title={isPaused ? 'Resume' : 'Pause'}
              style={{
                background: 'none', border: 'none', cursor: 'pointer',
                color: 'var(--text-secondary, #888)', fontSize: 16, padding: 2,
              }}
            >
              {isPaused ? '▶' : '⏸'}
            </button>
            <button
              onClick={onClose}
              title="Close"
              style={{
                background: 'none', border: 'none', cursor: 'pointer',
                color: 'var(--text-secondary, #888)', display: 'flex', padding: 2,
              }}
            >
              <X size={18} />
            </button>
          </div>
        </div>

        {/* ── Step content ── */}
        <div style={{ padding: '20px 24px 8px' }}>
          <div style={{ fontSize: 40, marginBottom: 8 }}>{step.icon}</div>
          <h2 style={{ fontSize: 22, fontWeight: 700, margin: '0 0 8px' }}>{step.title}</h2>
          <p style={{ fontSize: 14, lineHeight: 1.6, color: 'var(--text-secondary, #aaa)', margin: '0 0 16px' }}>
            {step.description}
          </p>

          {/* Tips */}
          <ul style={{ listStyle: 'none', padding: 0, margin: 0, display: 'flex', flexDirection: 'column', gap: 6 }}>
            {step.tips.map((tip, i) => (
              <li
                key={i}
                style={{
                  display: 'flex', alignItems: 'flex-start', gap: 8,
                  fontSize: 13, lineHeight: 1.5,
                  color: 'var(--text-primary, #ddd)',
                  padding: '6px 10px',
                  borderRadius: 8,
                  backgroundColor: 'var(--bg-canvas, rgba(255,255,255,0.04))',
                }}
              >
                <Sparkles size={14} style={{ marginTop: 3, flexShrink: 0, color: 'var(--accent-primary, #0F8A8A)' }} />
                <span>{tip}</span>
              </li>
            ))}
          </ul>
        </div>

        {/* ── Footer buttons ── */}
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', padding: '16px 24px 20px' }}>
          <button
            onClick={onSkip}
            style={{
              background: 'none', border: 'none', cursor: 'pointer',
              color: 'var(--text-secondary, #888)', fontSize: 13,
            }}
          >
            Skip guide
          </button>

          <div style={{ display: 'flex', gap: 8 }}>
            {!isFirst && (
              <button
                onClick={goPrev}
                style={{
                  display: 'flex', alignItems: 'center', gap: 4,
                  padding: '8px 14px', borderRadius: 8, fontSize: 13, fontWeight: 600,
                  cursor: 'pointer',
                  backgroundColor: 'transparent',
                  border: '1px solid var(--border-default, #444)',
                  color: 'var(--text-primary, #ddd)',
                }}
              >
                <ChevronLeft size={16} /> Back
              </button>
            )}
            <button
              onClick={goNext}
              style={{
                display: 'flex', alignItems: 'center', gap: 4,
                padding: '8px 18px', borderRadius: 8, fontSize: 13, fontWeight: 600,
                cursor: 'pointer', border: 'none',
                backgroundColor: 'var(--accent-primary, #0F8A8A)',
                color: '#fff',
              }}
            >
              {isLast ? (
                <><Check size={16} /> Get Started</>
              ) : (
                <>Next <ChevronRight size={16} /></>
              )}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

export default WalkthroughModal;
