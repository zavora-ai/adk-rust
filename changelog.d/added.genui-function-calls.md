- **A2UI 1.0 function calls** (`adk-surface`, feature `genui`): `read_function_message` reads
  `callRendererFunction`, `callAgentFunction` and their answers, checking what 1.0 requires,
  and `FunctionInvocation::refusal` writes the conformant answer: `INVALID_FUNCTION_CALL` with
  the reason (`forms` functions are `rendererOnly`; the agent offers none). `from_wire` refuses
  a function message in a drawing and names `read_function_message`.
