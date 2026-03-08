use crate::ServerConfig;
use adk_artifact::{ListRequest, LoadRequest};
use axum::{
    Extension, Json,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::IntoResponse,
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

fn authorize_user_id(
    request_context: &Option<adk_core::RequestContext>,
    user_id: &str,
) -> Result<String, StatusCode> {
    match request_context {
        Some(context) if context.user_id != user_id => Err(StatusCode::FORBIDDEN),
        Some(context) => Ok(context.user_id.clone()),
        None => Ok(user_id.to_string()),
    }
}

pub async fn list_artifacts(
    State(controller): State<ArtifactsController>,
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((app_name, user_id, session_id)): Path<(String, String, String)>,
) -> Result<Json<Vec<String>>, StatusCode> {
    let user_id = authorize_user_id(&request_context, &user_id)?;

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
    Extension(request_context): Extension<Option<adk_core::RequestContext>>,
    Path((app_name, user_id, session_id, artifact_name)): Path<(String, String, String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let user_id = authorize_user_id(&request_context, &user_id)?;

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
            adk_core::Part::Text { text } => {
                Ok(([(header::CONTENT_TYPE, mime_header)], Body::from(text)))
            }
            _ => Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
