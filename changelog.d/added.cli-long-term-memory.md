- **Long-term memory in `adk-rust`** (`adk-cli`, spec 3.1 §10 L2): the REPL, `code` and
  `goal` keep long-term facts in `harness.db`, distil a session when it ends, and recall
  facts in later sessions of the same project. `/memory` lists working notes and facts with
  status, kind, source and session; `/memory forget|accept ID` acts on one;
  `adk-rust memory list|show|forget|accept|export|import` manages them from the shell;
  `--no-memory` turns memory off. The first run says once what is stored and where.
