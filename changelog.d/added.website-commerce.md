- **AP2 payments and amount thresholds on the AWP website** (`adk-website`, `adk-payments`):
  `edge::ap2::Ap2Desk` takes payment for orders with the site as the AP2 merchant —
  `place_order` returns the site's signed cart, and `pay_order` with the buyer's payment
  mandate runs through `adk-payments`' `Ap2Adapter` and the owner's payment processor, with a
  receipt signed by the edge's `receipt` key (`SiteCartVerifier`, `SandboxProcessor`,
  `in_memory_journal`, `edge::adapters::ReceiptKey`). `pay_order` takes a `mandate` or a rail
  `credential`; `PaymentDesk` gains the provided `take_for` and `checkout`, the payment
  handler pays an order once, and `OrderBook::set_status` is public.
  `packs::ApprovalThresholds` (`SiteEdgeParts::thresholds`, `approvals.json` in a served site)
  holds `place_order`, `pay_order`, `book`, `book_appointment` and `donate` calls above the
  owner's amounts for a decision, reading the quote's or order's total and the new
  `Slot::price` (`Calendar::price`). `TeamKit::wallet` and `wallet::finance_tools` give the
  Finance agent `wallet_pay` and `wallet_status`. `adk_payments::protocol::ap2` re-exports
  `PaymentRequest`.
