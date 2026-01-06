import { LlmAgentNode } from './LlmAgentNode';
import { SequentialNode } from './SequentialNode';
import { LoopNode } from './LoopNode';
import { ParallelNode } from './ParallelNode';
import { RouterNode } from './RouterNode';
import { StartNode, EndNode } from './StartEndNodes';

export const nodeTypes = {
  llm: LlmAgentNode,
  sequential: SequentialNode,
  loop: LoopNode,
  parallel: ParallelNode,
  router: RouterNode,
  start: StartNode,
  end: EndNode,
};

export { LlmAgentNode, SequentialNode, LoopNode, ParallelNode, RouterNode, StartNode, EndNode };
