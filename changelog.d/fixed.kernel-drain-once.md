- **`Kernel::shutdown_on_signal` drains once** (`adk-kernel`): a signal stops the kernel
  after one 30-second drain, not two, and `stop` after a `drain` no longer drains again.
