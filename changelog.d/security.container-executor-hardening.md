- **Container executors stop runaway code and drop privileges** (`adk-code`):
  `ContainerCommandExecutor` names each container and runs `{runtime} kill {name}` when
  the timeout passes or the call is dropped; ending the `docker run` client had left the
  container running. `DockerExecutor` kills a timed-out exec's process group (or process)
  inside the persistent container, and discards output past the request's byte limits as
  it streams instead of buffering all of it. By default both run containers with every
  capability dropped, `no-new-privileges`, and a 256-process limit.
