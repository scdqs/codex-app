use std::sync::Arc;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicAccessMode {
    Local,
    Quick,
    Named,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicAccessContext {
    pub mode: PublicAccessMode,
    pub public_origin: Option<String>,
}

impl Default for PublicAccessContext {
    fn default() -> Self {
        Self {
            mode: PublicAccessMode::Local,
            public_origin: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    ForegroundOnly,
    WebPush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionState {
    Unavailable,
    NotEnabled,
    Active,
    NeedsRepair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationCapabilities {
    pub delivery_mode: DeliveryMode,
    pub fixed_https: bool,
    pub system_notifications: bool,
    pub foreground_sound: bool,
    pub foreground_vibration: bool,
    pub vibration_controlled_by_system: bool,
}

#[derive(Clone, Default)]
pub struct PublicAccessState(Arc<RwLock<PublicAccessContext>>);

impl PublicAccessState {
    pub async fn update(&self, context: PublicAccessContext) -> Result<()> {
        validate_context(&context)?;
        *self.0.write().await = context;
        Ok(())
    }

    pub async fn current(&self) -> PublicAccessContext {
        self.0.read().await.clone()
    }

    pub async fn notification_capabilities(&self) -> NotificationCapabilities {
        let context = self.current().await;
        NotificationCapabilities {
            delivery_mode: DeliveryMode::ForegroundOnly,
            fixed_https: context.mode == PublicAccessMode::Named
                && context
                    .public_origin
                    .as_deref()
                    .is_some_and(|origin| origin.starts_with("https://")),
            system_notifications: false,
            foreground_sound: true,
            foreground_vibration: true,
            vibration_controlled_by_system: false,
        }
    }
}

fn validate_context(context: &PublicAccessContext) -> Result<()> {
    match context.mode {
        PublicAccessMode::Local => Ok(()),
        PublicAccessMode::Quick | PublicAccessMode::Named => {
            let origin = context
                .public_origin
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("public origin is required"))?;
            let parsed = url::Url::parse(origin)?;
            if parsed.scheme() != "https"
                || parsed.origin().ascii_serialization() != origin
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
            {
                bail!("public origin must be an HTTPS origin");
            }
            if context.mode == PublicAccessMode::Quick
                && !parsed
                    .host_str()
                    .unwrap_or_default()
                    .ends_with("trycloudflare.com")
            {
                bail!("quick tunnel origin must use trycloudflare.com");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn named_origin_is_recorded_but_phase_two_delivery_stays_foreground_only() {
        let state = PublicAccessState::default();
        state
            .update(PublicAccessContext {
                mode: PublicAccessMode::Named,
                public_origin: Some("https://codex.example.com".into()),
            })
            .await
            .expect("context updates");

        let capabilities = state.notification_capabilities().await;
        assert!(capabilities.fixed_https);
        assert_eq!(capabilities.delivery_mode, DeliveryMode::ForegroundOnly);
        assert!(!capabilities.system_notifications);
    }
}
