import { useEffect, useRef, useState } from "react";
import { problemText } from "../client";
import type { AgentsSeam } from "./client";
import type { AgentPreview, AgentPreviewRequest } from "./types";

/** How long the form waits after the last keystroke before it asks for a new preview. */
const SETTLE_MS = 250;

/**
 * The kernel's preview of `request`, kept current as it changes (spec 3.3 §5.7 "Here is what
 * Finance can do"): the latest answer only, so a slow reply never replaces a newer one.
 */
export function usePreview(seam: AgentsSeam, request: AgentPreviewRequest | undefined) {
  const [preview, setPreview] = useState<AgentPreview>();
  const [problem, setProblem] = useState("");
  const [pending, setPending] = useState(false);
  const asked = useRef(0);
  const key = request ? JSON.stringify(request) : "";

  useEffect(() => {
    if (!request) {
      setPreview(undefined);
      setProblem("");
      return;
    }
    const ticket = ++asked.current;
    setPending(true);
    const timer = setTimeout(() => {
      seam
        .preview(request)
        .then((next) => {
          if (ticket !== asked.current) return;
          setPreview(next);
          setProblem("");
        })
        .catch((error: unknown) => {
          if (ticket !== asked.current) return;
          setPreview(undefined);
          setProblem(problemText(error));
        })
        .finally(() => {
          if (ticket === asked.current) setPending(false);
        });
    }, SETTLE_MS);
    return () => clearTimeout(timer);
    // The request is compared by value.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [seam, key]);

  // A preview of an earlier request is never confirmed: the digest binds what was shown.
  const current = !pending && preview !== undefined;
  return { preview, problem, pending, current };
}
