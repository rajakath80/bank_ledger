use crate::{AppState, auth::Auth, error::AppError};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct User {
    pub username: String,
    pub display_name: Option<String>,
    pub legal_name: Option<String>,
    pub email: Vec<String>,
    pub role: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateUser {
    pub username: String,
    pub display_name: String,
    pub legal_name: Option<String>,
    pub email: Option<String>,
    pub password: String,
    pub role: Option<Role>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUser {
    pub display_name: Option<String>,
    pub legal_name: Option<String>,
    pub email: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct SetRole {
    pub role: Role,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
}

impl Role {
    fn group<'a>(&self, state: &'a AppState) -> &'a str {
        match self {
            Role::Admin => &state.config.admin_group,
            Role::User => &state.config.user_group,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }
}

pub async fn health() -> Json<Value> {
    Json(serde_json::json!({"status": "ok"}))
}

pub async fn me(auth: Auth) -> Json<Value> {
    Json(serde_json::json!({
        "sub": auth.claims.sub,
        "preferred_username": auth.claims.preferred_username,
        "groups": auth.claims.groups
    }))
}

fn require_admin(auth: &Auth, state: &AppState) -> Result<(), AppError> {
    if auth.is_admin(&state.config.admin_group) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn entry_to_user(entry: &kanidm_proto::v1::Entry, admin_group: &str, user_group: &str) -> User {
    let get = |name: &str| entry.attrs.get(name).and_then(|v| v.first()).cloned();
    let email = entry.attrs.get("mail").cloned().unwrap_or_default();
    let memberof = entry.attrs.get("memberof").cloned().unwrap_or_default();
    let role = if memberof.iter().any(|g| g.contains(admin_group)) {
        "admin"
    } else if memberof.iter().any(|g| g.contains(user_group)) {
        "user"
    } else {
        "none"
    };

    User {
        username: get("name").unwrap_or_default(),
        display_name: get("displayname"),
        legal_name: get("legalname"),
        email,
        role: role.to_string(),
    }
}

pub async fn list_users(
    auth: Auth,
    State(state): State<AppState>,
) -> Result<Json<Vec<User>>, AppError> {
    require_admin(&auth, &state)?;
    let entries = state.kanidm.client.idm_person_account_list().await?;
    let users = entries
        .iter()
        .map(|e| entry_to_user(e, &state.config.admin_group, &state.config.user_group))
        .collect();
    Ok(Json(users))
}

pub async fn get_user(
    auth: Auth,
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<Json<User>, AppError> {
    require_admin(&auth, &state)?;
    let entry = state
        .kanidm
        .client
        .idm_person_account_get(&username)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(entry_to_user(
        &entry,
        &state.config.admin_group,
        &state.config.user_group,
    )))
}

pub async fn create_user(
    auth: Auth,
    State(state): State<AppState>,
    Json(req): Json<CreateUser>,
) -> Result<(StatusCode, Json<User>), AppError> {
    require_admin(&auth, &state)?;

    if req.username.trim().is_empty() || req.display_name.trim().is_empty() {
        return Err(AppError::BadRequest(
            "username and display_name are required".into(),
        ));
    }
    if req.password.len() < 12 {
        return Err(AppError::BadRequest(
            "password must be at least 12 characters".into(),
        ));
    }

    state
        .kanidm
        .client
        .idm_person_account_create(&req.username, &req.display_name)
        .await?;

    if let Some(legal_name) = &req.legal_name {
        state
            .kanidm
            .client
            .idm_person_account_update(&req.username, None, None, Some(legal_name), None)
            .await?;
    }
    if let Some(email) = &req.email {
        state
            .kanidm
            .client
            .idm_person_account_set_attr(&req.username, "mail", &[email.as_str()])
            .await?;
    }

    state
        .kanidm
        .client
        .idm_person_account_primary_credential_set_password(&req.username, &req.password)
        .await?;

    let role = req.role.unwrap_or(Role::User);
    state
        .kanidm
        .client
        .idm_group_add_members(role.group(&state), &[&req.username])
        .await?;

    let entry = state
        .kanidm
        .client
        .idm_person_account_get(&req.username)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok((
        StatusCode::CREATED,
        Json(entry_to_user(
            &entry,
            &state.config.admin_group,
            &state.config.user_group,
        )),
    ))
}

pub async fn update_user(
    auth: Auth,
    State(state): State<AppState>,
    Path(username): Path<String>,
    Json(req): Json<UpdateUser>,
) -> Result<Json<User>, AppError> {
    require_admin(&auth, &state)?;

    if state
        .kanidm
        .client
        .idm_person_account_get(&username)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }

    state
        .kanidm
        .client
        .idm_person_account_update(
            &username,
            None,
            req.display_name.as_deref(),
            req.legal_name.as_deref(),
            req.email.as_deref(),
        )
        .await?;

    let entry = state
        .kanidm
        .client
        .idm_person_account_get(&username)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(entry_to_user(
        &entry,
        &state.config.admin_group,
        &state.config.user_group,
    )))
}

pub async fn delete_user(
    auth: Auth,
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<StatusCode, AppError> {
    require_admin(&auth, &state)?;

    if state
        .kanidm
        .client
        .idm_person_account_get(&username)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }

    state
        .kanidm
        .client
        .idm_group_remove_members(&state.config.admin_group, &[&username])
        .await
        .ok();
    state
        .kanidm
        .client
        .idm_group_remove_members(&state.config.user_group, &[&username])
        .await
        .ok();
    state
        .kanidm
        .client
        .idm_person_account_delete(&username)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn set_role(
    auth: Auth,
    State(state): State<AppState>,
    Path(username): Path<String>,
    Json(req): Json<SetRole>,
) -> Result<Json<User>, AppError> {
    require_admin(&auth, &state)?;

    if state
        .kanidm
        .client
        .idm_person_account_get(&username)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }

    // A user has exactly one application role.
    state
        .kanidm
        .client
        .idm_group_remove_members(&state.config.admin_group, &[&username])
        .await
        .ok();
    state
        .kanidm
        .client
        .idm_group_remove_members(&state.config.user_group, &[&username])
        .await
        .ok();
    state
        .kanidm
        .client
        .idm_group_add_members(req.role.group(&state), &[&username])
        .await?;

    let entry = state
        .kanidm
        .client
        .idm_person_account_get(&username)
        .await?
        .ok_or(AppError::NotFound)?;

    Ok(Json(entry_to_user(
        &entry,
        &state.config.admin_group,
        &state.config.user_group,
    )))
}
