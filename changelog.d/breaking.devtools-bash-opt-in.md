- **`Workspace::new` disables `bash`** (`adk-devtools`): a new workspace offered `bash`,
  which runs model-written commands on the host outside the file tools' path containment.
  It is now off until `Workspace::allow_bash(true)`, and `DevToolset` omits the tool until
  then. The `adk-rust code`, `goal`, and `ultracode` commands enable it. A model-supplied
  `timeout_secs` can shorten a command's timeout but no longer extend it past
  `Workspace::bash_timeout`.
