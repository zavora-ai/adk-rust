import { Handle, Position } from '@xyflow/react';

const handleStyle = "!w-2 !h-2 !bg-gray-400";

export function StartNode() {
  return (
    <div className="rounded-lg px-4 py-2" style={{ background: '#1a472a', border: '2px solid #4ade80', color: '#fff' }}>
      <span>▶ START</span>
      <Handle type="source" position={Position.Bottom} id="bottom" className={handleStyle} />
      <Handle type="source" position={Position.Right} id="right" className={handleStyle} />
    </div>
  );
}

export function EndNode() {
  return (
    <div className="rounded-lg px-4 py-2" style={{ background: '#4a1a1a', border: '2px solid #f87171', color: '#fff' }}>
      <span>⏹ END</span>
      <Handle type="target" position={Position.Top} id="top" className={handleStyle} />
      <Handle type="target" position={Position.Left} id="left" className={handleStyle} />
    </div>
  );
}
