- **Web experiment analysis** (`adk-eval`, feature `experiments`; spec 3.5 §7.12):
  `ExperimentAnalyzer` analyses a control-versus-variant experiment look by look with an
  always-valid sequential test (normal-mixture boundary, fixed error budget) for proportions
  and means, reports differences and relative lifts with confidence sequences, and checks
  guardrails at every look: a proven harm beyond a guardrail's tolerance stops the experiment
  and keeps the control. `AnalysisMethod::Bayesian` adds posterior probabilities, expected loss
  and credible intervals for the primary metric. State serialises between looks
  (`ExperimentAnalyzer::resume`); `SequentialBoundary` exposes the boundary.
