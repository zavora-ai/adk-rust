- **A2A v1 `ListTasks` returns a `ListTasksResponse` and pages** (`adk-server`): `ListTasks`
  returned a bare array of tasks, and its page size only truncated the list. It now returns
  `{tasks, nextPageToken, pageSize, totalSize}` (A2A §3.1.4) on both bindings:
  - **Order:** most recently updated first.
  - **Page size:** 50 tasks per page by default, between 1 and 100.
  - **Paging:** an opaque `pageToken` continues the list, and `nextPageToken` is empty on
    the last page.
  - **Filters and contents:** `statusTimestampAfter`, `historyLength` and
    `includeArtifacts` now apply. Artifacts are left out unless asked for.
  - **Invalid input:** an out-of-range page size, a malformed token or timestamp, and an
    unknown REST `status` get `InvalidParams`.

  The push config list returns `{configs, nextPageToken}` in place of a bare array.
  `RequestHandler::tasks_list` and `CallerScope::tasks_list` return `TaskListResponse`.
  `A2aV1Client::list_tasks` and `list_tasks_rest` return `TaskListResponse` too, and still
  read the bare array older servers send. The push config list calls read both shapes.

  Migration: read `.tasks` from the result, and follow `next_page_token` for more.
