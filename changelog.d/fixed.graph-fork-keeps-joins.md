- **A fork keeps a waiting join's arrivals** (`adk-graph`): `TimeTravelHandle::fork_at`
  dropped the fan-in arrivals recorded at the forked checkpoint, so a join waiting there
  fired with only the arrivals after the fork. Forks now carry them.
