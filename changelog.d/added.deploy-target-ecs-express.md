- **Amazon ECS Express Mode deploy target** (`adk-deploy`, `cargo-adk`): feature `aws` adds
  `aws::EcsExpressClient`, which creates, describes, updates and deletes Express services over
  the documented ECS JSON API with Signature Version 4 and the AWS credential chain, and
  `aws::EcsExpressTarget`, behind `cargo adk deploy --target ecs-express`. Secrets come from
  Secrets Manager or Parameter Store references, `[scaling]` becomes the scaling target, and
  `[health]` the load balancer health check. `targets::verify_endpoint_within` waits for a new
  load balancer to pass its health check before verifying the endpoint.
