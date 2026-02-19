use crate::ServerConfig;
use adk_artifact::{ListRequest, LoadRequest};
use axum::{
    Json,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

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
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
    if let Some(service) = &controller.config.artifact_service {
        let resp = service
            .list(ListRequest { app_name, user_id, session_id })
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        Ok(Json(resp.file_names))
    } else {
        Ok(Json(vec![]))
    }
}

pub async fn get_artifact(
    State(controller): State<ArtifactsController>,
    Path((app_name, user_id, session_id, artifact_name)): Path<(String, String, String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    if let Some(service) = &controller.config.artifact_service {
        let resp = service
            .load(LoadRequest {
                app_name,
                user_id,
                session_id,
                file_name: artifact_name.clone(),
                version: None,
            })
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;

        let mime = mime_guess::from_path(&artifact_name).first_or_octet_stream();
        let mime_header = header::HeaderValue::from_str(mime.as_ref())
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));

        match resp.part {
            adk_core::Part::InlineData { data, .. } => {
                Ok(([(header::CONTENT_TYPE, mime_header)], Body::from(data)))
            }
            adk_core::Part::InlineDataBase64 { data_base64, .. } => {
                // Decode on demand only for byte-oriented artifact responses.
                let data = BASE64_STANDARD
                    .decode(data_base64)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
                Ok(([(header::CONTENT_TYPE, mime_header)], Body::from(data)))
            }
            adk_core::Part::Text { text } => {
                Ok(([(header::CONTENT_TYPE, mime_header)], Body::from(text)))
            }
            _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
