- **An order is its buyer's** (`adk-website`): `order_status` and `request_return` answer
  `resource_not_found` to any requester other than the one the order's quote was issued to,
  as `place_order` does; before, any known agent with an order id read its total and status
  and could ask for its return.
