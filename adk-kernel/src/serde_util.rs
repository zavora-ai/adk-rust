//! Durations on the wire as whole milliseconds, so `ps` output and the control plane's JSON
//! read as numbers rather than `{secs, nanos}` objects.

/// `Duration` as `u64` milliseconds.
pub(crate) mod millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        value: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(deserializer)?))
    }
}

/// `Option<Duration>` as optional `u64` milliseconds.
pub(crate) mod option_millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        value: &Option<Duration>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => {
                serializer.serialize_some(&u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
            }
            None => serializer.serialize_none(),
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Duration>, D::Error> {
        Ok(Option::<u64>::deserialize(deserializer)?.map(Duration::from_millis))
    }
}

/// `adk_core::RunBudget`, which has no serde form of its own, as
/// `{"maxModelCalls", "maxTotalTokens", "maxCostMicroUsd", "maxWallTimeMs", "maxToolCalls",
/// "allowUnpricedModels"}` with unset limits left out (spec 3.2 §7.5).
pub(crate) mod run_budget {
    use std::time::Duration;

    use adk_core::RunBudget;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize, Default)]
    #[serde(rename_all = "camelCase", default)]
    pub(crate) struct Wire {
        #[serde(skip_serializing_if = "Option::is_none")]
        max_model_calls: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_total_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_cost_micro_usd: Option<u64>,
        #[serde(rename = "maxWallTimeMs", skip_serializing_if = "Option::is_none")]
        max_wall_time_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_tool_calls: Option<u64>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        allow_unpriced_models: bool,
    }

    impl From<&RunBudget> for Wire {
        fn from(budget: &RunBudget) -> Self {
            Self {
                max_model_calls: budget.max_model_calls,
                max_total_tokens: budget.max_total_tokens,
                max_cost_micro_usd: budget.max_cost_micro_usd,
                max_wall_time_ms: budget
                    .max_wall_time
                    .map(|wall| u64::try_from(wall.as_millis()).unwrap_or(u64::MAX)),
                max_tool_calls: budget.max_tool_calls,
                allow_unpriced_models: budget.allow_unpriced_models,
            }
        }
    }

    impl From<Wire> for RunBudget {
        fn from(wire: Wire) -> Self {
            let mut budget = RunBudget::new();
            if let Some(max) = wire.max_model_calls {
                budget = budget.max_model_calls(max);
            }
            if let Some(max) = wire.max_total_tokens {
                budget = budget.max_total_tokens(max);
            }
            if let Some(max) = wire.max_cost_micro_usd {
                budget = budget.max_cost_micro_usd(max);
            }
            if let Some(max) = wire.max_wall_time_ms {
                budget = budget.max_wall_time(Duration::from_millis(max));
            }
            if let Some(max) = wire.max_tool_calls {
                budget = budget.max_tool_calls(max);
            }
            if wire.allow_unpriced_models {
                budget = budget.allow_unpriced_models();
            }
            budget
        }
    }

    pub(crate) fn serialize<S: Serializer>(
        budget: &RunBudget,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        Wire::from(budget).serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<RunBudget, D::Error> {
        Ok(Wire::deserialize(deserializer)?.into())
    }
}

/// `Option<adk_core::RunBudget>` in the form of [`run_budget`].
pub(crate) mod option_run_budget {
    use adk_core::RunBudget;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::run_budget::Wire;

    pub(crate) fn serialize<S: Serializer>(
        budget: &Option<RunBudget>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        budget.as_ref().map(Wire::from).serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<RunBudget>, D::Error> {
        Ok(Option::<Wire>::deserialize(deserializer)?.map(RunBudget::from))
    }
}
