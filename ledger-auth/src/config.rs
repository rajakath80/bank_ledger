use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_addr: String,
    pub kanidm_url: String,
    pub kanidm_api_token: String,
    pub oidc_issuer: String,
    pub oidc_audience: String,
    pub admin_group: String,
    pub user_group: String,
    pub accept_invalid_certs: bool,
    pub accept_invalid_hostnames: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let required = |name: &str| {
            env::var(name).map_err(|_| format!("missing required environment variable {name}"))
        };

        Ok(Self {
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into()),
            kanidm_url: required("KANIDM_URL")?.trim_end_matches('/').to_string(),
            kanidm_api_token: required("KANIDM_API_TOKEN")?,
            oidc_issuer: required("OIDC_ISSUER")?.trim_end_matches('/').to_string(),
            oidc_audience: required("OIDC_AUDIENCE")?,
            admin_group: env::var("KANIDM_ADMIN_GROUP").unwrap_or_else(|_| "app_admins".into()),
            user_group: env::var("KANIDM_USER_GROUP").unwrap_or_else(|_| "app_users".into()),
            accept_invalid_certs: env::var("KANIDM_ACCEPT_INVALID_CERTS")
                .unwrap_or_else(|_| "false".into()).parse().map_err(|e| format!("KANIDM_ACCEPT_INVALID_CERTS: {e}"))?,
            accept_invalid_hostnames: env::var("KANIDM_ACCEPT_INVALID_HOSTNAMES")
                .unwrap_or_else(|_| "false".into()).parse().map_err(|e| format!("KANIDM_ACCEPT_INVALID_HOSTNAMES: {e}"))?,
        })
    }
}
