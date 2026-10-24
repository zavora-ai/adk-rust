- **`[kernel] drivers` in `harness.toml`** (`adk-cli`, `adk-kernel`, spec 3.2 §7.7, §8.2):
  the user config lists driver manifests — paths to files or directories of them, or inline
  manifests — and `adk-rust up` mounts them when the kernel starts. Each driver not mounted
  exactly as written is shown and mounted only once the person approves it at the terminal;
  one already approved keeps its approval and pinned declarations across restarts, and a
  workspace's `.adk/drivers/<id>.toml` may only restrict it. The key is no longer reported as
  unknown. `DriverSupervisor::is_mounted_as` and `DriverSupervisor::mount_at_start` are new.
