- **A2A v1 push notifications are delivered** (`adk-server`): push notification configs
  were stored but no notification was ever sent. When the agent card offers push
  notifications, each status and artifact update of a running task now goes to every push
  config on the task through the handler's `PushNotificationSender`, until the task ends or
  stops for its caller. Configs added while the task runs are included. A push config sent
  with `SendMessage` (`taskPushNotificationConfig`) is saved on the task first. A failed
  delivery is logged and does not fail the task.

  It is opt-in: `A2aV1Server::builder(..).push_sender(HttpPushNotificationSender::new())`
  turns it on and declares it on the card. Without a sender, the card says
  `pushNotifications: false` and push configs are refused.
