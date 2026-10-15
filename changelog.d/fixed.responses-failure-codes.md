- **Responses failures keep OpenAI's documented error codes** (`adk-model`): a failed
  response, `response.failed`, or `error` event whose code is one of OpenAI's documented
  `response.error.code` values records it in `provider_error_code` and the message, and
  sets the category: `rate_limit_exceeded` is `RateLimited` and `server_error` is
  `Unavailable`, both retryable, `vector_store_timeout` is `Timeout`, and input codes are
  `InvalidInput`. Other codes and provider messages stay out of the error. A cancelled
  response has the `Cancelled` category.
