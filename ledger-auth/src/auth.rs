use crate::{AppState, error::AppError};
use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iss: String,
    pub aud: String,
    pub exp: usize,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Clone)]
pub struct JwksCache {
    pub jwks_uri: String,
    pub keys: Arc<RwLock<(Instant, jsonwebtoken::jwk::JwkSet)>>,
    pub client: reqwest::Client,
}

impl JwksCache {
    pub async fn new(issuer: &str) -> Result<Self, AppError> {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .map_err(|e| AppError::Internal(e.to_string()))?;

        let discovery_url = format!("{issuer}/.well-known/openid-configuration");
        let discovery: serde_json::Value = client
            .get(&discovery_url)
            .send()
            .await
            .map_err(|e| AppError::Identity(e.to_string()))?
            .error_for_status()
            .map_err(|e| AppError::Identity(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::Identity(e.to_string()))?;

        let jwks_uri = discovery["jwks_uri"]
            .as_str()
            .ok_or_else(|| AppError::Identity("OIDC discovery has no jwks_uri".into()))?
            .to_string();

        let jwks = client
            .get(&jwks_uri)
            .send()
            .await
            .map_err(|e| AppError::Identity(e.to_string()))?
            .error_for_status()
            .map_err(|e| AppError::Identity(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::Identity(e.to_string()))?;

        Ok(Self {
            jwks_uri,
            keys: Arc::new(RwLock::new((Instant::now(), jwks))),
            client,
        })
    }

    async fn refresh(&self) -> Result<(), AppError> {
        let jwks = self
            .client
            .get(&self.jwks_uri)
            .send()
            .await
            .map_err(|e| AppError::Identity(e.to_string()))?
            .error_for_status()
            .map_err(|e| AppError::Identity(e.to_string()))?
            .json()
            .await
            .map_err(|e| AppError::Identity(e.to_string()))?;
        *self.keys.write().await = (Instant::now(), jwks);
        Ok(())
    }

    async fn key_for(&self, kid: &str) -> Result<DecodingKey, AppError> {
        {
            let guard = self.keys.read().await;
            if let Some(jwk) = guard
                .1
                .keys
                .iter()
                .find(|k| k.common.key_id.as_deref() == Some(kid))
            {
                return DecodingKey::from_jwk(jwk).map_err(|e| AppError::Internal(e.to_string()));
            }
        }
        self.refresh().await?;
        let guard = self.keys.read().await;
        let jwk = guard
            .1
            .keys
            .iter()
            .find(|k| k.common.key_id.as_deref() == Some(kid))
            .ok_or(AppError::Unauthorized)?;
        DecodingKey::from_jwk(jwk).map_err(|e| AppError::Internal(e.to_string()))
    }
}

#[derive(Clone)]
pub struct Auth {
    pub claims: Claims,
}

impl Auth {
    pub fn is_admin(&self, group: &str) -> bool {
        self.claims.groups.iter().any(|g| g == group)
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let token = header
            .strip_prefix("Bearer ")
            .ok_or(AppError::Unauthorized)?;

        let header = decode_header(token).map_err(|_| AppError::Unauthorized)?;
        let alg = match header.alg {
            Algorithm::ES256 => Algorithm::ES256,
            Algorithm::RS256 => Algorithm::RS256,
            _ => return Err(AppError::Unauthorized),
        };

        let kid = header.kid.ok_or(AppError::Unauthorized)?;

        // JWK discovery is cached for this process. Refresh occurs when a key id is not found.
        let cache = state.kanidm.jwks.clone();

        let key = cache.key_for(&kid).await?;

        let mut validation = Validation::new(alg);
        validation.set_issuer(&[state.config.oidc_issuer.as_str()]);
        validation.set_audience(&[state.config.oidc_audience.as_str()]);

        let token_data =
            decode::<Claims>(token, &key, &validation).map_err(|_| AppError::Unauthorized)?;

        Ok(Auth {
            claims: token_data.claims,
        })
    }
}
