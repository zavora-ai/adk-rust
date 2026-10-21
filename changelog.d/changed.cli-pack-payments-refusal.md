- **`adk-rust --pack payments` is refused with the library path** (`adk-cli`, spec 3.1
  §7.16): the pack needs a merchant checkout service, a transaction store and a mandate,
  which no flag or config file can supply; the message names
  `HarnessAgentBuilder::pack_payments(PaymentsPack)`, the `harness` template that
  scaffolds a binary, and the payments guide.
