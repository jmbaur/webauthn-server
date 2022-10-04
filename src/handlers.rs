use crate::app;
use app::SharedAppState;
use async_trait::async_trait;
use axum::extract::{FromRequest, Path, Query, RequestParts};
use axum::response::{Html, Redirect};
use axum::{extract, http::StatusCode, Extension, Json};
use axum_macros::debug_handler;
use axum_sessions::extractors::{ReadableSession, WritableSession};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use webauthn_rs::prelude::{PasskeyAuthentication, PasskeyRegistration};
use webauthn_rs::Webauthn;
use webauthn_rs_proto::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse,
};

const SESSIONKEY_LOGGEDIN: &str = "logged_in";
const SESSIONKEY_PASSKEYREGISTRATION: &str = "passkey_registration";
const SESSIONKEY_PASSKEYAUTHENTICATION: &str = "passkey_authentication";
const SESSIONKEY_REDIRECTURL: &str = "redirect_url";

pub struct RequireLoggedIn;

#[async_trait]
impl<B> FromRequest<B> for RequireLoggedIn
where
    B: Send,
{
    type Rejection = Redirect;

    // TODO(jared): On top of ensuring the client's cookie is associated with a session where the
    // user is logged in, should we also ensure that the username for the session matches with the
    // authenticated user? This would mean that a user who exists in the database could not use the
    // cookie from another user in order to pass authentication. Currently, only the username value
    // from the X-Remote-User header is used for fetching a users information, however it could use
    // the username value from the existing session. This really only makes sense if the username
    // is validated with the existing session.
    async fn from_request(req: &mut RequestParts<B>) -> Result<Self, Self::Rejection> {
        tracing::trace!("RequireLoggedIn extractor");

        if let Ok(session) = ReadableSession::from_request(req).await {
            if session.get::<bool>(SESSIONKEY_LOGGEDIN).unwrap_or_default() {
                return Ok(Self);
            }
        }

        Err(Redirect::temporary(
            "/authenticate?redirect_url=/credentials",
        ))
    }
}

pub struct XRemoteUser(String);

#[async_trait]
impl<B> FromRequest<B> for XRemoteUser
where
    B: Send,
{
    type Rejection = StatusCode;

    async fn from_request(req: &mut RequestParts<B>) -> Result<Self, Self::Rejection> {
        tracing::trace!("XRemoteUser extractor");

        if let Some(x_remote_user) = req.headers().get("X-Remote-User") {
            if let Ok(val) = x_remote_user.to_str() {
                Ok(XRemoteUser(String::from(val)))
            } else {
                tracing::error!("could not conver header value to string");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        } else {
            tracing::info!("no X-Remote-User header present");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

#[debug_handler]
pub async fn validate_handler(session: ReadableSession) -> StatusCode {
    if session.get::<bool>(SESSIONKEY_LOGGEDIN).unwrap_or_default() {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}

#[debug_handler]
pub async fn register_start_handler(
    XRemoteUser(username): XRemoteUser,
    mut session: WritableSession,
    shared_state: Extension<SharedAppState>,
    webauthn: Extension<Arc<Webauthn>>,
) -> Result<Json<CreationChallengeResponse>, StatusCode> {
    tracing::trace!("register_start_handler");

    let app = shared_state.read().await;
    let user = app.get_user_with_credentials(username).await?;

    let (req_chal, passkey_reg) = match webauthn.start_passkey_registration(
        user.id,
        &user.username,
        &user.username, // use username as display name
        Some(
            user.credentials
                .iter()
                .map(|c| c.credential.cred_id().to_owned())
                .collect(),
        ),
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("webauthn.start_passkey_registration: {e}");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    if let Err(e) = session.insert(SESSIONKEY_PASSKEYREGISTRATION, passkey_reg) {
        tracing::error!("session.insert: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    };

    Ok(Json(req_chal))
}

#[derive(Serialize, Deserialize)]
pub struct RegisterEndRequestPayload {
    name: String,
    credential: RegisterPublicKeyCredential,
}

#[debug_handler]
pub async fn register_end_handler(
    XRemoteUser(username): XRemoteUser,
    mut session: WritableSession,
    payload: extract::Json<RegisterEndRequestPayload>,
    shared_state: Extension<SharedAppState>,
    webauthn: Extension<Arc<Webauthn>>,
) -> Result<(), StatusCode> {
    tracing::trace!("register_end_handler");

    let app = shared_state.read().await;

    let passkey_reg = match session.get::<PasskeyRegistration>(SESSIONKEY_PASSKEYREGISTRATION) {
        Some(s) => s,
        _ => return Err(StatusCode::NOT_FOUND),
    };

    let passkey = match webauthn.finish_passkey_registration(&payload.credential, &passkey_reg) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("webauthn.finish_passkey_registration: {e}");
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    let user = app.get_user_with_credentials(username.clone()).await?;

    if user
        .credentials
        .iter()
        .any(|c| *c.credential.cred_id() == *passkey.cred_id())
    {
        tracing::error!("credential already registered");
        return Err(StatusCode::BAD_REQUEST);
    }

    app.add_credential(username, payload.name.clone(), passkey)
        .await?;

    session.remove(SESSIONKEY_PASSKEYREGISTRATION);

    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct AuthenticateStartResponsePayload {
    challenge: Option<RequestChallengeResponse>,
}

#[debug_handler]
pub async fn authenticate_start_handler(
    XRemoteUser(username): XRemoteUser,
    mut session: WritableSession,
    shared_state: Extension<SharedAppState>,
    webauthn: Extension<Arc<Webauthn>>,
) -> Result<Json<AuthenticateStartResponsePayload>, StatusCode> {
    tracing::trace!("authenticate_start_handler");

    if session.get::<bool>(SESSIONKEY_LOGGEDIN).unwrap_or_default() {
        tracing::debug!("user already logged in");
        return Ok(Json(AuthenticateStartResponsePayload { challenge: None }));
    }

    let user = shared_state
        .read()
        .await
        .get_user_with_credentials(username.clone())
        .await?;

    if user.credentials.is_empty() {
        tracing::debug!("user does not have any credentials");
        if let Err(e) = session.insert(SESSIONKEY_LOGGEDIN, true) {
            tracing::error!("session.insert: {e}");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        return Ok(Json(AuthenticateStartResponsePayload { challenge: None }));
    }

    let passkeys: Vec<_> = user
        .credentials
        .iter()
        .map(|c| c.credential.to_owned())
        .collect();

    let (req_chal, passkey_auth) = match webauthn.start_passkey_authentication(&passkeys) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("webauthn.start_passkey_authentication: {e}");
            return Err(StatusCode::UNAUTHORIZED);
        }
    };

    if let Err(e) = session.insert(SESSIONKEY_PASSKEYAUTHENTICATION, passkey_auth) {
        tracing::error!("session.insert: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(Json(AuthenticateStartResponsePayload {
        challenge: Some(req_chal),
    }))
}

#[derive(Serialize)]
pub struct AuthenticateEndResponsePayload {
    pub redirect_url: String,
}

#[debug_handler]
pub async fn authenticate_end_handler(
    mut session: WritableSession,
    payload: extract::Json<PublicKeyCredential>,
    shared_state: Extension<SharedAppState>,
    webauthn: Extension<Arc<Webauthn>>,
) -> Result<Json<AuthenticateEndResponsePayload>, StatusCode> {
    tracing::trace!("authenticate_end_handler");

    let passkey_authentication =
        match session.get::<PasskeyAuthentication>(SESSIONKEY_PASSKEYAUTHENTICATION) {
            Some(a) => a,
            None => return Err(StatusCode::NO_CONTENT),
        };

    let auth_result =
        match webauthn.finish_passkey_authentication(&payload.0, &passkey_authentication) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("webauthn.finish_passkey_authentication: {e}");
                return Err(StatusCode::UNAUTHORIZED);
            }
        };

    let state = shared_state.read().await;
    if auth_result.needs_update() {
        state.update_credential(auth_result).await?;
    }

    session.remove(SESSIONKEY_PASSKEYAUTHENTICATION);
    if let Err(e) = session.insert(SESSIONKEY_LOGGEDIN, true) {
        tracing::error!("session.insert: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    if let Some(redirect_url) = session.get::<String>(SESSIONKEY_REDIRECTURL) {
        session.remove(SESSIONKEY_REDIRECTURL);
        Ok(Json(AuthenticateEndResponsePayload { redirect_url }))
    } else {
        Ok(Json(AuthenticateEndResponsePayload {
            redirect_url: format!("{}/{}", state.origin, "credentials"),
        }))
    }
}

#[derive(Serialize, Deserialize)]
pub struct GetCredentialsResponsePayload {
    pub data: Vec<CredentialIDWithName>,
}

#[debug_handler]
pub async fn delete_credentials_api_handler(
    Path(cred_id): Path<String>,
    shared_state: Extension<SharedAppState>,
) -> Result<StatusCode, StatusCode> {
    tracing::trace!("delete_credentials_handler");

    let app = shared_state.read().await;
    app.delete_credential(cred_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(RustEmbed)]
#[folder = "templates"]
struct Templates;

#[derive(Serialize, Deserialize, Debug)]
pub struct CredentialIDWithName {
    id: String,
    name: String,
}

#[debug_handler]
pub async fn get_credentials_template_handler(
    XRemoteUser(username): XRemoteUser,
    parser: Extension<Arc<liquid::Parser>>,
    shared_state: Extension<SharedAppState>,
) -> (StatusCode, Html<String>) {
    if let Some(template) = Templates::get("credentials.liquid") {
        let parsed_template =
            match parser.parse(std::str::from_utf8(&template.data).unwrap_or_default()) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("parser.parse: {e}");
                    return (StatusCode::INTERNAL_SERVER_ERROR, Html(String::new()));
                }
            };

        let app = shared_state.read().await;
        if let Ok(user) = app.get_user_with_credentials(username).await {
            let credentials: Vec<CredentialIDWithName> = dbg!(user
                .credentials
                .iter()
                .map(|c| {
                    let c = c.clone();
                    CredentialIDWithName {
                        id: c.credential.cred_id().to_string(),
                        name: c.name,
                    }
                })
                .collect());

            let globals = liquid::object!({
                "credentials": credentials,
            });

            if let Ok(output) = parsed_template.render(&globals) {
                (StatusCode::OK, Html(output))
            } else {
                (StatusCode::INTERNAL_SERVER_ERROR, Html(String::new()))
            }
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, Html(String::new()))
        }
    } else {
        // TODO(jared): don't do this
        (StatusCode::NOT_FOUND, Html(String::new()))
    }
}

#[derive(Deserialize)]
pub struct GetAuthenticateQueryParams {
    pub redirect_url: Option<String>,
}

#[debug_handler]
pub async fn get_authenticate_template_handler(
    params: Query<GetAuthenticateQueryParams>,
    XRemoteUser(username): XRemoteUser,
    mut session: WritableSession,
    parser: Extension<Arc<liquid::Parser>>,
    shared_state: Extension<SharedAppState>,
    webauthn: Extension<Arc<Webauthn>>,
) -> (StatusCode, Html<String>) {
    if let Some(template) = Templates::get("authenticate.liquid") {
        let parsed_template =
            match parser.parse(std::str::from_utf8(&template.data).unwrap_or_default()) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("parser.parse: {e}");
                    return (StatusCode::INTERNAL_SERVER_ERROR, Html(String::new()));
                }
            };
        let globals = liquid::object!({
            "username": username,
        });
        let state = shared_state.read().await;
        let redirect_url = params
            .redirect_url
            .clone()
            .unwrap_or_else(|| format!("{}/{}", state.origin, "credentials"));
        let url = match url::Url::parse(&redirect_url) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!("url::Url::parse: {e}");
                return (StatusCode::BAD_REQUEST, Html(String::new()));
            }
        };
        if !webauthn
            .get_allowed_origins()
            .iter()
            .any(|u| u.origin() == url.origin())
        {
            tracing::info!("denied client request for redirect to {redirect_url}");
            return (StatusCode::FORBIDDEN, Html(String::new()));
        }
        if let Ok(output) = parsed_template.render(&globals) {
            if let Err(e) = session.insert(SESSIONKEY_REDIRECTURL, redirect_url) {
                tracing::error!("session.insert: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, Html(String::new()));
            }
            (StatusCode::OK, Html(output))
        } else {
            (StatusCode::INTERNAL_SERVER_ERROR, Html(String::new()))
        }
    } else {
        // TODO(jared): don't do this
        (StatusCode::NOT_FOUND, Html(String::new()))
    }
}

#[cfg(test)]
mod tests {
    use webauthn_rs::prelude::*;
    use webauthn_rs::WebauthnBuilder;

    #[test]
    fn exploration() {
        let webauthn =
            WebauthnBuilder::new("foo.com", &Url::parse("https://auth.foo.com").unwrap())
                .unwrap()
                .allow_subdomains(true)
                .append_allowed_origin(&Url::parse("https://foo.com").unwrap())
                .append_allowed_origin(&Url::parse("https://bar.foo.com").unwrap())
                .build()
                .unwrap();

        // passes
        [
            "https://bar.foo.com",
            "https://auth.foo.com",
            "https://foo.com",
        ]
        .iter()
        .for_each(|&url| {
            assert!(webauthn
                .get_allowed_origins()
                .iter()
                .any(|u| u.origin() == Url::parse(url).unwrap().origin()));
        });

        // fails
        ["https://fo.com", "https://foo.bar.com"]
            .iter()
            .for_each(|&url| {
                assert!(webauthn
                    .get_allowed_origins()
                    .iter()
                    .any(|u| u.origin() != Url::parse(url).unwrap().origin()));
            });
    }
}
