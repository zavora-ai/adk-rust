import { motion, AnimatePresence } from 'framer-motion';

interface ThoughtBubbleProps {
  text: string;
  position?: 'top' | 'right';
  streaming?: boolean;
  type?: 'thinking' | 'tool' | 'decision';
}

const icons = { thinking: '💭', tool: '🔧', decision: '🤔' };

const colors = {
  thinking: 'from-blue-500 to-blue-600',
  tool: 'from-yellow-500 to-yellow-600',
  decision: 'from-purple-500 to-purple-600',
};

export function ThoughtBubble({ 
  text, 
  position = 'right', 
  streaming = false,
  type = 'thinking'
}: ThoughtBubbleProps) {
  if (!text) return null;

  const positionClasses = {
    right: 'left-full ml-3 top-1/2 -translate-y-1/2',
    top: 'bottom-full mb-3 left-1/2 -translate-x-1/2',
  };

  const pointerClasses = {
    right: 'left-0 top-1/2 -translate-x-full -translate-y-1/2 border-r-blue-500 border-y-transparent border-l-transparent border-8',
    top: 'bottom-0 left-1/2 translate-y-full -translate-x-1/2 border-t-blue-500 border-x-transparent border-b-transparent border-8',
  };

  return (
    <AnimatePresence>
      <motion.div
        className={`absolute z-50 ${positionClasses[position]}`}
        initial={{ opacity: 0, scale: 0.8 }}
        animate={{ opacity: 1, scale: 1 }}
        exit={{ opacity: 0, scale: 0.8 }}
        transition={{ duration: 0.15 }}
      >
        <div className={`relative bg-gradient-to-br ${colors[type]} rounded-lg px-3 py-2 max-w-[250px] shadow-lg`}>
          <div className={`absolute ${pointerClasses[position]}`} style={{ width: 0, height: 0 }} />
          <div className="flex items-start gap-2 text-white text-xs">
            <span>{icons[type]}</span>
            <span className="leading-relaxed">
              {text}
              {streaming && <span className="animate-pulse ml-1">▊</span>}
            </span>
          </div>
        </div>
      </motion.div>
    </AnimatePresence>
  );
}
