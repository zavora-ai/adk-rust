#[cfg(feature = "vertex")]
mod check {
    use google_cloud_aiplatform_v1::client::{
        PredictionService,
        JobService,
        GenAiCacheService,
        LlmUtilityService,
        DatasetService,
        ModelService
    };
    use google_cloud_auth::credentials::CredentialsFile;
}

fn main() {}
