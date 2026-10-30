- **`ModelProvider::default_model` returns `Option<&'static str>`** (`adk-model`): `None` for
  `Bedrock` and `AzureAi`, whose model or deployment ID depends on the account, region or
  endpoint, as `catalog::recommended_model` reports. Exhaustive matches on `ModelProvider` need
  arms for the six new variants.
