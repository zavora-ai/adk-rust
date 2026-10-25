- **`FileConsentService` writes atomically off the async executor** (`adk-awp`, spec 3.5 §10
  X3): each capture or revocation wrote the whole file in place on the executor thread, so a
  reader or a crash could see a partial file. Each write now goes to a synced temporary file
  beside the target that is renamed over it on a blocking thread, and writes are serialised so
  the file always ends with the latest state.
