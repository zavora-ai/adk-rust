- **A debug build of `adk-rust up` no longer overflows a worker's stack** (`adk-cli`): the
  runtime's worker threads run on the same 8 MiB stack as the commands, where they had the
  2 MiB default. With the `website` feature, an `/agent` change on the kernel overflowed it
  and aborted the kernel.
