use crate::ServerConfig;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};

#[derive(Clone)]
pub struct ArtifactsController {
    config: ServerConfig,
}

impl ArtifactsController {
    pub fn new(config: ServerConfig) -> Self {
        Self { config }
    }
}

pub async fn list_artifacts(
    State(controller): State<ArtifactsController>,
    Path((_app_name, _user_id, _session_id)): Path<(String, String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
    if let Some(service) = &controller.config.artifact_service {
        let artifacts = service
            .list()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Json(artifacts))
    } else {
        Ok(Json(vec![]))
    }
}

pub async fn get_artifact(
    State(controller): State<ArtifactsController>,
    Path((_app_name, _user_id, _session_id, artifact_name)): Path<(String, String, String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    if let Some(service) = &controller.config.artifact_service {
        let content = service
            .load(&artifact_name)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;

        let mime = mime_guess::from_path(&artifact_name).first_or_octet_stream();
        let mime_header = header::HeaderValue::from_str(mime.as_ref()).unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));

        match content {
            adk_core::Part::InlineData { data, .. } => Ok((
                [(header::CONTENT_TYPE, mime_header)],
                Body::from(data),
            )),
            adk_core::Part::Text { text } => Ok((
                [(header::CONTENT_TYPE, mime_header)],
                Body::from(text),
            )),
            _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
