//! The chat platforms of a served site (spec 3.5 §10 X12): the adapters of the project's
//! `channels.json`, built here because `adk-website` cannot depend on `adk-surface`, handed to
//! the site as its `SiteOptions::chat`. The site runs one gateway over them — a paired
//! account's messages are the Steward's, a stranger's the Inbox's — and its public edge
//! serves WhatsApp's webhook at `/channels/whatsapp`.

use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

use adk_kernel::channels::{ChannelGateway, ChannelSink};
use adk_website::deploy::chat::{ChatAdapters, WHATSAPP_PATH};
use adk_website::deploy::config::ChatConfig;
use anyhow::Result;
use async_trait::async_trait;

/// The adapters `channels.json` names, and what they run once started.
pub(super) struct Adapters {
    telegram: Option<adk_surface::channels::TelegramAdapter>,
    #[cfg(feature = "whatsapp")]
    whatsapp: Option<adk_surface::channels::WhatsAppAdapter>,
    #[cfg(feature = "slack")]
    slack: Option<adk_surface::channels::SlackAdapter>,
    #[cfg(feature = "discord")]
    discord: Option<adk_surface::channels::DiscordAdapter>,
    #[cfg(feature = "matrix")]
    matrix: Option<adk_surface::channels::MatrixAdapter>,
    /// The pollings, sockets and webhook handles, kept while the site serves.
    running: Mutex<Vec<Box<dyn Any + Send + Sync>>>,
}

impl Adapters {
    /// The adapters of `config`.
    ///
    /// # Errors
    ///
    /// A malformed credential or address, or a platform this build has no adapter for.
    pub(super) fn build(config: &ChatConfig) -> Result<Self> {
        let unbuilt = |platform: &str| {
            anyhow::anyhow!(
                "channels.json names {platform}, which this build of adk-rust has no adapter for"
            )
        };
        let telegram = match &config.telegram {
            Some(token) => {
                Some(adk_surface::channels::TelegramAdapter::builder(token.expose()).build()?)
            }
            None => None,
        };
        #[cfg(feature = "whatsapp")]
        let whatsapp = match &config.whatsapp {
            Some(w) => {
                let mut builder = adk_surface::channels::WhatsAppAdapter::builder(
                    &w.phone_number_id,
                    w.access_token.expose(),
                )
                .app_secret(w.app_secret.expose())
                .verify_token(w.verify_token.expose());
                if let Some((name, language)) = &w.template {
                    builder = builder.template(name, language);
                }
                Some(builder.build()?)
            }
            None => None,
        };
        #[cfg(not(feature = "whatsapp"))]
        if config.whatsapp.is_some() {
            return Err(unbuilt("WhatsApp"));
        }
        #[cfg(feature = "slack")]
        let slack = match &config.slack {
            Some((app, bot)) => Some(
                adk_surface::channels::SlackAdapter::builder(app.expose(), bot.expose()).build()?,
            ),
            None => None,
        };
        #[cfg(not(feature = "slack"))]
        if config.slack.is_some() {
            return Err(unbuilt("Slack"));
        }
        #[cfg(feature = "discord")]
        let discord = match &config.discord {
            Some(token) => {
                Some(adk_surface::channels::DiscordAdapter::builder(token.expose()).build()?)
            }
            None => None,
        };
        #[cfg(not(feature = "discord"))]
        if config.discord.is_some() {
            return Err(unbuilt("Discord"));
        }
        #[cfg(feature = "matrix")]
        let matrix = match &config.matrix {
            Some((homeserver, token)) => Some(
                adk_surface::channels::MatrixAdapter::builder(homeserver, token.expose())
                    .build()?,
            ),
            None => None,
        };
        #[cfg(not(feature = "matrix"))]
        if config.matrix.is_some() {
            return Err(unbuilt("Matrix"));
        }
        let _ = unbuilt;
        Ok(Self {
            telegram,
            #[cfg(feature = "whatsapp")]
            whatsapp,
            #[cfg(feature = "slack")]
            slack,
            #[cfg(feature = "discord")]
            discord,
            #[cfg(feature = "matrix")]
            matrix,
            running: Mutex::default(),
        })
    }

    fn keep(&self, handle: impl Any + Send + Sync) {
        self.running.lock().unwrap_or_else(PoisonError::into_inner).push(Box::new(handle));
    }
}

#[async_trait]
impl ChatAdapters for Adapters {
    fn sinks(&self) -> Vec<Arc<dyn ChannelSink>> {
        let mut sinks: Vec<Arc<dyn ChannelSink>> = Vec::new();
        if let Some(telegram) = &self.telegram {
            sinks.push(Arc::new(telegram.clone()));
        }
        #[cfg(feature = "whatsapp")]
        if let Some(whatsapp) = &self.whatsapp {
            sinks.push(Arc::new(whatsapp.clone()));
        }
        #[cfg(feature = "slack")]
        if let Some(slack) = &self.slack {
            sinks.push(Arc::new(slack.clone()));
        }
        #[cfg(feature = "discord")]
        if let Some(discord) = &self.discord {
            sinks.push(Arc::new(discord.clone()));
        }
        #[cfg(feature = "matrix")]
        if let Some(matrix) = &self.matrix {
            sinks.push(Arc::new(matrix.clone()));
        }
        sinks
    }

    async fn start(&self, gateway: ChannelGateway) -> adk_core::Result<axum::Router> {
        let mut routes = axum::Router::new();
        if let Some(telegram) = &self.telegram {
            self.keep(telegram.start(gateway.clone()));
        }
        #[cfg(feature = "whatsapp")]
        if let Some(whatsapp) = &self.whatsapp {
            let webhook = whatsapp.start(gateway.clone());
            routes = routes.nest(WHATSAPP_PATH, webhook.router());
            self.keep(webhook);
        }
        #[cfg(feature = "slack")]
        if let Some(slack) = &self.slack {
            self.keep(slack.start(gateway.clone()));
        }
        #[cfg(feature = "discord")]
        if let Some(discord) = &self.discord {
            self.keep(discord.start(gateway.clone()));
        }
        #[cfg(feature = "matrix")]
        if let Some(matrix) = &self.matrix {
            self.keep(matrix.start(gateway.clone()));
        }
        let _ = (&gateway, WHATSAPP_PATH);
        Ok(routes)
    }
}

#[cfg(all(test, feature = "whatsapp"))]
mod tests {
    use std::time::Duration;

    use adk_website::deploy::config::chat;
    use adk_website::deploy::{ProjectPaths, ServeOptions, serve};
    use adk_website::onboarding::scaffold::{ScaffoldOptions, scaffold};
    use adk_website::themes::ThemeId;

    use super::*;

    /// Spec 3.5 §10 X12: the project's `channels.json` names WhatsApp, its secrets in files;
    /// the served site's public edge answers the webhook's handshake at `/channels/whatsapp`
    /// and refuses a body that is not signed with the app secret.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_edge_serves_whatsapps_webhook_and_refuses_a_bad_signature() {
        let dir = tempfile::tempdir().unwrap();
        let options = ScaffoldOptions {
            name: "Juniper Bakery".into(),
            domain: "juniper.example".into(),
            kind: awp_types::SiteKind::Business,
            languages: vec!["en".parse().unwrap()],
            theme: ThemeId::Paper,
            currency: "KES".into(),
        };
        scaffold(dir.path(), &options).await.unwrap();
        for (file, secret) in
            [("token", "EAAG-test"), ("app-secret", "app-secret"), ("verify", "verify-me")]
        {
            std::fs::write(dir.path().join(file), secret).unwrap();
        }
        let config = serde_json::json!({ "whatsapp": {
            "phoneNumberId": "1001",
            "accessToken": "file:token",
            "appSecret": "file:app-secret",
            "verifyToken": "file:verify",
        }});
        std::fs::write(dir.path().join("channels.json"), config.to_string()).unwrap();
        let config = chat(&ProjectPaths::new(dir.path())).unwrap().expect("channels.json");
        assert_eq!(config.platforms(), ["WhatsApp"]);
        assert!(!format!("{config:?}").contains("app-secret"));

        let mut serving = ServeOptions::new(dir.path(), "owner");
        serving.edge = "127.0.0.1:0".parse().unwrap();
        serving.kernel = "127.0.0.1:0".parse().unwrap();
        serving.site.idle = Some(Duration::from_millis(100));
        serving.site.chat = Some(Arc::new(Adapters::build(&config).unwrap()));
        let served = serve(serving).await.unwrap();
        assert!(served.chat().is_some(), "the site runs a chat gateway");
        let edge = format!("http://{}{WHATSAPP_PATH}", served.edge_addr);
        let client = reqwest::Client::new();
        let handshake = client
            .get(format!("{edge}?hub.mode=subscribe&hub.verify_token=verify-me&hub.challenge=42"))
            .send()
            .await
            .unwrap();
        assert_eq!(handshake.status().as_u16(), 200);
        assert_eq!(handshake.text().await.unwrap(), "42");
        let forged = client
            .post(&edge)
            .header("x-hub-signature-256", format!("sha256={}", "0".repeat(64)))
            .body(r#"{"object":"whatsapp_business_account","entry":[]}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(forged.status().as_u16(), 401);
        let unsigned = client.post(&edge).body("{}").send().await.unwrap();
        assert_eq!(unsigned.status().as_u16(), 401);
        served.shutdown().await.unwrap();
    }
}
