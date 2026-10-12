use std::sync::Arc;

use crate::{auth::JwksCache, config::Config, error::AppError};
use kanidm_client::{KanidmClient, KanidmClientBuilder};

#[derive(Clone)]
pub struct Kanidm {
    pub client: Arc<KanidmClient>,
    pub jwks: JwksCache,
}

impl Kanidm {
    pub async fn new(config: &Config) -> Result<Self, AppError> {
        let client = KanidmClientBuilder::new()
            .address(config.kanidm_url.clone())
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .danger_accept_invalid_hostnames(config.accept_invalid_hostnames)
            .build()
            .map_err(AppError::from)?;

        // Kanidm service-account API tokens are accepted as HTTP Bearer credentials.
        client.set_token(config.kanidm_api_token.clone()).await;

        let jwks = JwksCache::new(&config.oidc_issuer).await?;

        Ok(Self {
            client: Arc::new(client),
            jwks,
        })
    }

    pub async fn ensure_role_groups(&self, config: &Config) -> Result<(), AppError> {
        for group in [&config.admin_group, &config.user_group] {
            if self.client.idm_group_get(group).await?.is_none() {
                self.client.idm_group_create(group, None).await?;
            }
        }
        Ok(())
    }
}
