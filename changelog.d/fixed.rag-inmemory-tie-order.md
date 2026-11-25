- **`InMemoryVectorStore` returns equal scores in one order** (`adk-rag`): results of equal
  score came back in hash-map order, which changes from one process to the next, so a query
  over identical chunks — many identical video captions, for example — cited different chunks
  on each run. Equal scores now rank by ascending chunk ID; results of different scores keep
  their order.
