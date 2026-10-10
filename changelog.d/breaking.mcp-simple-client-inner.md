- **`SimpleClient::inner` returns the shared connection** (`adk-tool`): it returned
  `&Arc<Mutex<RunningService<RoleClient, S>>>` and now returns
  `&Arc<RunningService<RoleClient, S>>`, since requests no longer go through a mutex.
  Replace `client.inner().lock().await` with `client.inner()`.
