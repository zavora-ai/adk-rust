- **The coordinator's crossings into other partitions' drivers** (`adk-kernel`, `adk-harness`,
  `adk-cli`; spec 3.2 §7.7, §7.8, §13 Q3, K-I15): `HarnessAgentBuilder::partition_driver_crossings`
  offers the coordinator each tool another partition's own harness is served, as the crossing
  `<partition>__<tool>` (`DriverSupervisor::crossing_toolset`), read from the partition table at
  every listing. Each call is the coordinator's crossing under a non-waivable approval: a read of
  a tool the manifest classes `read`, with its approved declaration, holds for the rest of the
  run; every other class is approved one call at a time; a run-wide approval naming such a tool
  is removed when the run starts. A crossing is classed and governed by the driver policy as the
  driver tool it wraps, so a stale tool and `publish_public` and `financial` are asked every
  call. `adk-rust up` with `[[kernel.partitions]]` gives the coordinator every other partition's
  drivers this way. `examples/harness_kernel`: `k7_driver_crossings` scripted and
  `live_k7_driver_crossing` through `adk-rust up`.
