- **A kernel files every held call in its inbox and on its bus** (`adk-core`, `adk-kernel`):
  `KernelBuilder::approval_service` gives a service built without a router the `operator`
  route `adk-rust up` uses, and publishes `held` and `decided` for code-built kernels too.
  `ApprovalService::route_unrouted` and `ApprovalService::add_notifier` are the new defaulted
  trait methods `StoreApprovalService` implements; a service that takes neither is named in the
  kernel inventory as `kernel.approvals.unrouted` or `kernel.approvals.unheard`.
