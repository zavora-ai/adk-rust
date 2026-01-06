export type LayoutDirection = 'TB' | 'LR' | 'BT' | 'RL';
export type LayoutMode = 'pipeline' | 'tree' | 'cluster' | 'freeform';

export interface LayoutConfig {
  direction: LayoutDirection;
  nodeSpacing: number;
  rankSpacing: number;
}

export interface GraphAnalysis {
  nodeCount: number;
  edgeCount: number;
  maxDepth: number;
  hasCycles: boolean;
  dominantPattern: LayoutMode;
  entryPoints: string[];
  exitPoints: string[];
}
