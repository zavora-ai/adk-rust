//! Adapt MRTR input to the handler that owns the running connection.

use rmcp::{
    RoleClient,
    model::{
        ClientResult, GetExtensions, GetMeta, InputRequest, InputRequests, InputResponses,
        NumberOrString, ServerRequest,
    },
    service::{RequestContext, RunningService, Service},
};

pub(super) async fn fulfill<S: Service<RoleClient>>(
    client: &RunningService<RoleClient, S>,
    requests: InputRequests,
) -> Result<InputResponses, String> {
    let responses =
        futures::future::try_join_all(requests.into_iter().map(|(key, input)| async move {
            let mut request = match &input {
                InputRequest::Elicitation(request) => ServerRequest::ElicitRequest(request.clone()),
                InputRequest::CreateMessage(request) => {
                    ServerRequest::CreateMessageRequest(request.clone())
                }
                InputRequest::ListRoots(request) => {
                    ServerRequest::ListRootsRequest(request.clone())
                }
                _ => return Err("unsupported MCP input request".into()),
            };
            let mut context = RequestContext::new(
                NumberOrString::String(key.clone().into()),
                client.peer().clone(),
            );
            std::mem::swap(&mut context.meta, request.get_meta_mut());
            std::mem::swap(&mut context.extensions, request.extensions_mut());
            let result = client
                .service()
                .handle_request(request, context)
                .await
                .map_err(|error| error.to_string())?;
            let value = match (input, result) {
                (InputRequest::Elicitation(_), ClientResult::ElicitResult(result)) => {
                    serde_json::to_value(result)
                }
                (InputRequest::CreateMessage(_), ClientResult::CreateMessageResult(result)) => {
                    serde_json::to_value(result)
                }
                (InputRequest::ListRoots(_), ClientResult::ListRootsResult(result)) => {
                    serde_json::to_value(result)
                }
                _ => return Err("unexpected MCP input response".into()),
            }
            .map_err(|error| error.to_string())?;
            Ok::<_, String>((key, value))
        }))
        .await?;
    Ok(responses.into_iter().collect())
}
