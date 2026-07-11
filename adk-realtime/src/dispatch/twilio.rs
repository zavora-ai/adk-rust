use crate::{
    dispatch::{
        CallControlEvent, CallControlProvider, CallHandle, OriginateContext, RedirectTarget,
    },
    error::{RealtimeError, Result},
};
use async_trait::async_trait;
use futures::Stream;
use futures::StreamExt;
use reqwest::Client;
use std::pin::Pin;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

/// Call control provider for Twilio.
pub struct TwilioCallControlProvider {
    client: Client,
    account_sid: String,
    auth_token: String,
    from_number: String,
    event_tx: broadcast::Sender<(String, CallControlEvent)>,
}

impl TwilioCallControlProvider {
    /// Create a new TwilioCallControlProvider.
    pub fn new(account_sid: &str, auth_token: &str, from_number: &str) -> Self {
        let (event_tx, _) = broadcast::channel(100);
        Self {
            client: Client::new(),
            account_sid: account_sid.to_string(),
            auth_token: auth_token.to_string(),
            from_number: from_number.to_string(),
            event_tx,
        }
    }

    /// Checks a Twilio API response for a non-2xx status, returning the
    /// response body as error context instead of silently treating any
    /// completed request as success.
    async fn check_success(response: reqwest::Response) -> Result<reqwest::Response> {
        let status = response.status();
        if !status.is_success() {
            let err_text = response.text().await.unwrap_or_default();
            return Err(RealtimeError::provider(format!(
                "Twilio API returned error status {}: {}",
                status, err_text
            )));
        }
        Ok(response)
    }

    /// Handle an incoming Twilio webhook event.
    /// This should be called by the application's webhook handler.
    pub fn handle_webhook(
        &self,
        call_sid: &str,
        event_type: &str,
        params: &serde_json::Value,
    ) -> Result<()> {
        let event = match event_type {
            "ringing" => CallControlEvent::Ringing,
            "answered" => CallControlEvent::Answered,
            "completed" => CallControlEvent::Ended {
                reason: params
                    .get("CallStatus")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
            },
            "dtmf" => {
                let digit = params.get("Digits").and_then(|v| v.as_str()).unwrap_or("").to_string();
                CallControlEvent::Dtmf { digit }
            }
            _ => return Ok(()),
        };

        let _ = self.event_tx.send((call_sid.to_string(), event));
        Ok(())
    }
}

#[async_trait]
impl CallControlProvider for TwilioCallControlProvider {
    async fn originate(&self, phone_number: &str, context: OriginateContext) -> Result<CallHandle> {
        let url =
            format!("https://api.twilio.com/2010-04-01/Accounts/{}/Calls.json", self.account_sid);

        let mut params = vec![("To", phone_number.to_string()), ("From", self.from_number.clone())];

        if let Some(extra) = context.extra {
            if let Some(url) = extra.get("url").and_then(|v| v.as_str()) {
                params.push(("Url", url.to_string()));
            }
            if let Some(twiml) = extra.get("twiml").and_then(|v| v.as_str()) {
                params.push(("Twiml", twiml.to_string()));
            }
        }

        let response = self
            .client
            .post(&url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&params)
            .send()
            .await
            .map_err(|e| RealtimeError::provider(format!("Twilio API error: {}", e)))?;

        let response = Self::check_success(response).await?;

        let data: serde_json::Value = response
            .json()
            .await
            .map_err(|e| RealtimeError::provider(format!("Twilio JSON error: {}", e)))?;

        let call_sid = data
            .get("sid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| RealtimeError::provider("Missing call SID in Twilio response"))?;

        Ok(CallHandle { provider_call_id: call_sid.to_string(), room_name: None })
    }

    fn events(
        &self,
        handle: &CallHandle,
    ) -> Pin<Box<dyn Stream<Item = Result<CallControlEvent>> + Send>> {
        let call_id = handle.provider_call_id.clone();
        let rx = self.event_tx.subscribe();

        let stream = BroadcastStream::new(rx).filter_map(move |res| {
            let call_id = call_id.clone();
            async move {
                match res {
                    Ok((id, event)) if id == call_id => Some(Ok(event)),
                    _ => None,
                }
            }
        });

        Box::pin(stream)
    }

    async fn redirect(&self, handle: &CallHandle, destination: RedirectTarget) -> Result<()> {
        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls/{}.json",
            self.account_sid, handle.provider_call_id
        );

        let params = match destination {
            RedirectTarget::Url(u) => vec![("Url", u)],
            RedirectTarget::PhoneNumber(_) => {
                return Err(RealtimeError::provider(
                    "Twilio redirect to PhoneNumber requires TwiML",
                ));
            }
        };

        let response = self
            .client
            .post(&url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&params)
            .send()
            .await
            .map_err(|e| RealtimeError::provider(format!("Twilio redirect error: {}", e)))?;

        Self::check_success(response).await?;

        Ok(())
    }

    async fn hangup(&self, handle: &CallHandle) -> Result<()> {
        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls/{}.json",
            self.account_sid, handle.provider_call_id
        );

        let params = [("Status", "completed")];

        let response = self
            .client
            .post(&url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&params)
            .send()
            .await
            .map_err(|e| RealtimeError::provider(format!("Twilio hangup error: {}", e)))?;

        Self::check_success(response).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_twilio_event_translation() {
        let provider = TwilioCallControlProvider::new("AC123", "token", "+1234567890");
        let handle = CallHandle { provider_call_id: "CA123".to_string(), room_name: None };

        let mut events = provider.events(&handle);

        // Inject an event
        provider.handle_webhook("CA123", "answered", &json!({})).unwrap();

        let event = events.next().await.unwrap().unwrap();
        match event {
            CallControlEvent::Answered => {}
            _ => panic!("Expected Answered event"),
        }

        // Inject another event
        provider.handle_webhook("CA123", "completed", &json!({"CallStatus": "busy"})).unwrap();

        let event = events.next().await.unwrap().unwrap();
        match event {
            CallControlEvent::Ended { reason } => assert_eq!(reason, "busy"),
            _ => panic!("Expected Ended event"),
        }
    }

    #[tokio::test]
    async fn test_twilio_event_filtering() {
        let provider = TwilioCallControlProvider::new("AC123", "token", "+1234567890");
        let handle1 = CallHandle { provider_call_id: "CA1".to_string(), room_name: None };
        let handle2 = CallHandle { provider_call_id: "CA2".to_string(), room_name: None };

        let mut events1 = provider.events(&handle1);
        let mut events2 = provider.events(&handle2);

        provider.handle_webhook("CA1", "ringing", &json!({})).unwrap();
        provider.handle_webhook("CA2", "answered", &json!({})).unwrap();

        let e1 = events1.next().await.unwrap().unwrap();
        assert!(matches!(e1, CallControlEvent::Ringing));

        let e2 = events2.next().await.unwrap().unwrap();
        assert!(matches!(e2, CallControlEvent::Answered));
    }
}
