- **A served site keeps its edge's events and counts across a restart** (`adk-website`):
  `deploy::serve` keeps retained events, catalogue pages, consent references, pending
  deliveries, beacon counts and the measurement meters' counts in `data/edge.db`, and resumes
  the deliveries a stopped process left; all were in memory (spec 3.5 §7.9, §7.12).
