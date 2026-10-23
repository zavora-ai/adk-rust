- Responses selects HTTP when the final adapted request enables background mode,
  preserving streaming and background fields. The standalone WebSocket encoder
  rejects background requests instead of discarding their mode.

- The first HTTP fallback reuses the WebSocket attempt's adapted body and headers.
  Later HTTP retry attempts adapt the original request again without accumulating
  body or header changes.
