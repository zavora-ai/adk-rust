- **The harness OS sandbox no longer reads credentials through its toolchain grants**
  (`adk-harness`): `CARGO_HOME` is granted entry by entry (`bin`, `registry`, `git`, cargo's
  lock and cache files, and `config.toml` only when it holds no token), so
  `credentials.toml` stays unreadable; `RUSTUP_HOME`, `PYENV_ROOT` and `NVM_DIR` are granted
  the same way, and npm's global `npmrc` inside an nvm Node version stays out. A `PATH`
  directory under the home directory grants only its executable files and the files their
  links resolve to, never its tree or a file named like a credential, so a projects
  directory on `PATH` no longer exposes other projects' `.env` files.
