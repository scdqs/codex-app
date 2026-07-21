use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use web_push::{
    ContentEncoding, IsahcWebPushClient, SubscriptionInfo, VapidSignatureBuilder, WebPushClient,
    WebPushError, WebPushMessageBuilder,
};

use crate::{
    notification_store::PushSubscriptionRecord,
    protocol::{AlertEvent, AlertKind},
    vapid::VapidRuntimeKey,
};

const PUSH_TTL_SECONDS: u32 = 300;
const PUSH_SEND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PushPayload {
    pub event_id: String,
    pub kind: AlertKind,
    pub thread_id: String,
    pub thread_title: String,
    pub occurred_at: u64,
    pub vibration_enabled: bool,
    pub vibration_pattern: Vec<u16>,
    pub silent: bool,
    pub force_system_notification: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryHints {
    pub sound_enabled: bool,
    pub vibration_enabled: bool,
    pub force_system_notification: bool,
}

impl PushPayload {
    pub fn for_event(event: &AlertEvent, hints: DeliveryHints) -> Self {
        let vibration_pattern = if hints.vibration_enabled {
            match event.kind {
                AlertKind::Completed => vec![80],
                AlertKind::ApprovalRequired => vec![80, 60, 80],
                AlertKind::InputRequired => vec![45, 40, 45],
                AlertKind::Error => vec![150, 80, 150],
            }
        } else {
            Vec::new()
        };
        Self {
            event_id: event.event_id.clone(),
            kind: event.kind,
            thread_id: event.thread_id.clone(),
            thread_title: event.thread_title.clone(),
            occurred_at: event.occurred_at,
            vibration_enabled: hints.vibration_enabled,
            vibration_pattern,
            silent: !hints.sound_enabled,
            force_system_notification: hints.force_system_notification,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushFailureClass {
    InvalidSubscription,
    Retryable,
    Permanent,
}

impl PushFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSubscription => "push_invalid_subscription",
            Self::Retryable => "push_retryable",
            Self::Permanent => "push_permanent",
        }
    }
}

#[derive(Debug, Error)]
pub enum WebPushTransportError {
    #[error("push endpoint returned HTTP {0}")]
    HttpStatus(u16),
    #[error("push request timed out")]
    Timeout,
    #[error("push network request failed")]
    Network,
    #[error("push subscription material is invalid")]
    InvalidSubscriptionMaterial,
    #[error("VAPID key material is invalid")]
    InvalidVapidKey,
    #[error("push payload is too large")]
    PayloadTooLarge,
}

#[async_trait]
pub trait WebPushTransport: Send + Sync {
    async fn send(
        &self,
        subscription: &PushSubscriptionRecord,
        payload: &[u8],
        vapid_private_key_base64: &str,
    ) -> Result<(), WebPushTransportError>;
}

#[derive(Clone)]
pub struct WebPushSender {
    transport: Arc<dyn WebPushTransport>,
    vapid_key: Arc<VapidRuntimeKey>,
}

impl WebPushSender {
    pub fn new(transport: Arc<dyn WebPushTransport>, vapid_key: Arc<VapidRuntimeKey>) -> Self {
        Self {
            transport,
            vapid_key,
        }
    }

    pub async fn send(
        &self,
        subscription: &PushSubscriptionRecord,
        payload: &PushPayload,
    ) -> Result<(), PushFailureClass> {
        let bytes = serde_json::to_vec(payload).map_err(|_| PushFailureClass::Permanent)?;
        self.transport
            .send(subscription, &bytes, self.vapid_key.private_key_base64())
            .await
            .map_err(classify_web_push_error)
    }
}

#[derive(Clone)]
pub struct RustWebPushTransport {
    client: IsahcWebPushClient,
}

impl RustWebPushTransport {
    pub fn new() -> Result<Self, WebPushTransportError> {
        Ok(Self {
            client: IsahcWebPushClient::new().map_err(|_| WebPushTransportError::Network)?,
        })
    }
}

#[async_trait]
impl WebPushTransport for RustWebPushTransport {
    async fn send(
        &self,
        subscription: &PushSubscriptionRecord,
        payload: &[u8],
        vapid_private_key_base64: &str,
    ) -> Result<(), WebPushTransportError> {
        let subscription_info = SubscriptionInfo::new(
            subscription.endpoint.clone(),
            subscription.p256dh.clone(),
            subscription.auth.clone(),
        );
        let signature =
            VapidSignatureBuilder::from_base64(vapid_private_key_base64, &subscription_info)
                .map_err(map_vapid_error)?
                .build()
                .map_err(map_vapid_error)?;
        let mut builder = WebPushMessageBuilder::new(&subscription_info);
        builder.set_ttl(PUSH_TTL_SECONDS);
        builder.set_payload(ContentEncoding::Aes128Gcm, payload);
        builder.set_vapid_signature(signature);
        let message = builder.build().map_err(map_message_error)?;
        tokio::time::timeout(PUSH_SEND_TIMEOUT, self.client.send(message))
            .await
            .map_err(|_| WebPushTransportError::Timeout)?
            .map_err(map_send_error)
    }
}

pub fn classify_web_push_error(error: WebPushTransportError) -> PushFailureClass {
    match error {
        WebPushTransportError::HttpStatus(404 | 410)
        | WebPushTransportError::InvalidSubscriptionMaterial => {
            PushFailureClass::InvalidSubscription
        }
        WebPushTransportError::Timeout
        | WebPushTransportError::Network
        | WebPushTransportError::HttpStatus(408 | 429 | 500..=599) => PushFailureClass::Retryable,
        WebPushTransportError::HttpStatus(_)
        | WebPushTransportError::InvalidVapidKey
        | WebPushTransportError::PayloadTooLarge => PushFailureClass::Permanent,
    }
}

fn map_vapid_error(error: WebPushError) -> WebPushTransportError {
    match error {
        WebPushError::InvalidUri => WebPushTransportError::InvalidSubscriptionMaterial,
        WebPushError::PayloadTooLarge => WebPushTransportError::PayloadTooLarge,
        _ => WebPushTransportError::InvalidVapidKey,
    }
}

fn map_message_error(error: WebPushError) -> WebPushTransportError {
    match error {
        WebPushError::PayloadTooLarge => WebPushTransportError::PayloadTooLarge,
        WebPushError::InvalidUri
        | WebPushError::MissingCryptoKeys
        | WebPushError::InvalidCryptoKeys => WebPushTransportError::InvalidSubscriptionMaterial,
        _ => WebPushTransportError::Network,
    }
}

fn map_send_error(error: WebPushError) -> WebPushTransportError {
    match error {
        WebPushError::EndpointNotFound(info)
        | WebPushError::EndpointNotValid(info)
        | WebPushError::Unauthorized(info)
        | WebPushError::BadRequest(info)
        | WebPushError::NotImplemented(info)
        | WebPushError::Other(info) => WebPushTransportError::HttpStatus(info.code),
        WebPushError::ServerError { info, .. } => WebPushTransportError::HttpStatus(info.code),
        WebPushError::PayloadTooLarge => WebPushTransportError::PayloadTooLarge,
        WebPushError::InvalidUri
        | WebPushError::MissingCryptoKeys
        | WebPushError::InvalidCryptoKeys => WebPushTransportError::InvalidSubscriptionMaterial,
        WebPushError::InvalidClaims => WebPushTransportError::InvalidVapidKey,
        WebPushError::Unspecified
        | WebPushError::Io(_)
        | WebPushError::InvalidResponse
        | WebPushError::ResponseTooLarge => WebPushTransportError::Network,
        WebPushError::InvalidPackageName
        | WebPushError::InvalidTtl
        | WebPushError::InvalidTopic => WebPushTransportError::HttpStatus(400),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::*;

    #[test]
    fn push_payload_contains_only_allowed_alert_fields_and_delivery_hints() {
        let payload = PushPayload::for_event(
            &alert(AlertKind::Error),
            DeliveryHints {
                sound_enabled: true,
                vibration_enabled: true,
                force_system_notification: false,
            },
        );
        let value = serde_json::to_value(payload).expect("payload serializes");

        assert_eq!(
            value
                .as_object()
                .expect("payload is object")
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "eventId".into(),
                "kind".into(),
                "threadId".into(),
                "threadTitle".into(),
                "occurredAt".into(),
                "vibrationEnabled".into(),
                "vibrationPattern".into(),
                "silent".into(),
                "forceSystemNotification".into(),
            ])
        );
        assert!(!value.to_string().contains("cwd"));
        assert!(!value.to_string().contains("reply"));
        assert_eq!(value["vibrationPattern"], json!([150, 80, 150]));
    }

    #[test]
    fn classifies_push_errors_for_retry_and_invalidation() {
        assert_eq!(
            classify_web_push_error(WebPushTransportError::HttpStatus(410)),
            PushFailureClass::InvalidSubscription
        );
        assert_eq!(
            classify_web_push_error(WebPushTransportError::HttpStatus(404)),
            PushFailureClass::InvalidSubscription
        );
        assert_eq!(
            classify_web_push_error(WebPushTransportError::HttpStatus(429)),
            PushFailureClass::Retryable
        );
        assert_eq!(
            classify_web_push_error(WebPushTransportError::HttpStatus(503)),
            PushFailureClass::Retryable
        );
        assert_eq!(
            classify_web_push_error(WebPushTransportError::HttpStatus(400)),
            PushFailureClass::Permanent
        );
        assert_eq!(
            classify_web_push_error(WebPushTransportError::Timeout),
            PushFailureClass::Retryable
        );
    }

    fn alert(kind: AlertKind) -> AlertEvent {
        AlertEvent {
            event_id: "alert-1".into(),
            kind,
            thread_id: "thread-1".into(),
            thread_title: "Release".into(),
            occurred_at: 1,
        }
    }
}
