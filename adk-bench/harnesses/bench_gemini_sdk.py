#!/usr/bin/env python3
"""
External Benchmark Protocol (EBP) harness for the raw Google Gemini Python SDK.

Measures the overhead of google-generativeai when executing the same workload
as adk-bench. This is the "bare SDK" baseline — no agent framework, just direct
API calls with tool definitions.

Usage:
    python3 bench_gemini_sdk.py <workload.json>

Requires:
    - GOOGLE_API_KEY environment variable
    - BENCH_START_EPOCH_NS environment variable (injected by adk-bench)
    - pip install google-generativeai
"""

import json
import os
import sys
import time


def main():
    if len(sys.argv) < 2:
        print("Usage: bench_gemini_sdk.py <workload.json>", file=sys.stderr)
        sys.exit(1)

    workload_path = sys.argv[-1]
    api_key = os.environ.get("GOOGLE_API_KEY")
    bench_start_ns = int(os.environ.get("BENCH_START_EPOCH_NS", "0"))

    if not api_key:
        print("Error: GOOGLE_API_KEY not set", file=sys.stderr)
        sys.exit(1)

    # Load workload
    with open(workload_path) as f:
        workload = json.load(f)

    import google.generativeai as genai

    genai.configure(api_key=api_key)

    # Build tool definitions from workload
    tools = []
    tool_responses = {}
    for tool_name, tool_def in workload.get("agent", {}).get("tools", {}).items():
        # Build function declaration for Gemini
        params = tool_def.get("parameters", {})
        tools.append(genai.protos.Tool(
            function_declarations=[
                genai.protos.FunctionDeclaration(
                    name=tool_name,
                    description=tool_def.get("description", ""),
                    parameters=_schema_to_proto(params),
                )
            ]
        ))
        if "fixedResponse" in tool_def:
            tool_responses[tool_name] = tool_def["fixedResponse"]

    # Configure model with deterministic settings
    model = genai.GenerativeModel(
        model_name=workload.get("model", "gemini-2.5-flash"),
        tools=tools if tools else None,
        generation_config=genai.GenerationConfig(
            temperature=0.0,
            top_p=1.0,
        ),
        system_instruction=workload["agent"]["instructions"],
    )

    # Start chat and measure
    chat = model.start_chat()
    user_message = workload["agent"]["userMessage"]

    overhead_samples = []
    first_llm_call_ns = 0
    total_turns = 0

    # Execute the agent loop
    for turn in range(workload.get("expectedTurns", 5)):
        turn_start = time.perf_counter_ns()

        if turn == 0:
            first_llm_call_ns = time.time_ns()
            response = chat.send_message(user_message)
        else:
            # Send tool results from previous turn
            if pending_tool_calls:
                parts = []
                for fc in pending_tool_calls:
                    result = tool_responses.get(fc.name, {"status": "success"})
                    parts.append(genai.protos.Part(
                        function_response=genai.protos.FunctionResponse(
                            name=fc.name,
                            response={"result": result},
                        )
                    ))
                response = chat.send_message(parts)
            else:
                break

        llm_end = time.perf_counter_ns()
        total_turns += 1

        # Check for tool calls
        pending_tool_calls = []
        for part in response.parts:
            if hasattr(part, "function_call") and part.function_call.name:
                pending_tool_calls.append(part.function_call)

        # Simulate tool execution latency
        tool_time_ns = 0
        for fc in pending_tool_calls:
            tool_name = fc.name
            latency_ms = 0
            for tn, td in workload.get("agent", {}).get("tools", {}).items():
                if tn == tool_name:
                    latency_ms = td.get("simulatedLatencyMs", 0)
                    break
            if latency_ms > 0:
                time.sleep(latency_ms / 1000.0)
                tool_time_ns += latency_ms * 1_000_000

        turn_end = time.perf_counter_ns()
        turn_total_ns = turn_end - turn_start
        llm_time_ns = llm_end - turn_start
        # Overhead = total - llm - tool_simulation
        overhead_ns = turn_total_ns - llm_time_ns - tool_time_ns
        overhead_samples.append(max(0, overhead_ns // 1000))  # Convert to μs

        if not pending_tool_calls:
            break

    # Compute stats
    if not overhead_samples:
        overhead_samples = [0]

    overhead_samples.sort()
    count = len(overhead_samples)
    min_us = overhead_samples[0]
    max_us = overhead_samples[-1]
    mean_us = sum(overhead_samples) // count
    median_us = overhead_samples[count // 2]
    p95_idx = min(int(0.95 * count + 0.5), count) - 1
    p99_idx = min(int(0.99 * count + 0.5), count) - 1
    p95_us = overhead_samples[max(0, p95_idx)]
    p99_us = overhead_samples[max(0, p99_idx)]

    # Compute cold start
    cold_start_us = (first_llm_call_ns - bench_start_ns) // 1000 if bench_start_ns > 0 else 0

    # Get memory (RSS)
    try:
        import resource
        peak_rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        # macOS reports bytes, Linux reports KB
        if sys.platform == "darwin":
            peak_rss_bytes = peak_rss
        else:
            peak_rss_bytes = peak_rss * 1024
    except Exception:
        peak_rss_bytes = None

    # Output EBP JSON
    result = {
        "framework": "gemini-python-sdk",
        "cold_start_us": max(0, cold_start_us),
        "first_llm_call_epoch_ns": first_llm_call_ns,
        "loop_overhead": {
            "min_us": min_us,
            "max_us": max_us,
            "mean_us": mean_us,
            "median_us": median_us,
            "p95_us": p95_us,
            "p99_us": p99_us,
            "count": count,
        },
        "peak_rss_bytes": peak_rss_bytes,
        "throughput_agents_per_sec": None,
        "token_overhead": None,
    }

    print(json.dumps(result))


def _schema_to_proto(schema):
    """Convert a JSON Schema dict to a Gemini Schema proto (simplified)."""
    import google.generativeai as genai

    if not schema or schema == {}:
        return None

    # Simple conversion — handles basic object schemas
    schema_type = schema.get("type", "object")
    if schema_type == "object":
        properties = {}
        for prop_name, prop_schema in schema.get("properties", {}).items():
            prop_type = prop_schema.get("type", "string")
            type_map = {
                "string": genai.protos.Type.STRING,
                "integer": genai.protos.Type.INTEGER,
                "number": genai.protos.Type.NUMBER,
                "boolean": genai.protos.Type.BOOLEAN,
                "array": genai.protos.Type.ARRAY,
            }
            properties[prop_name] = genai.protos.Schema(
                type=type_map.get(prop_type, genai.protos.Type.STRING),
                description=prop_schema.get("description", ""),
            )

        return genai.protos.Schema(
            type=genai.protos.Type.OBJECT,
            properties=properties,
            required=schema.get("required", []),
        )
    return None


if __name__ == "__main__":
    main()
