use std::collections::HashMap;

use actix_web::{get, post, web, HttpResponse};
use askama::Template;
use chrono::TimeZone;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Config;

pub fn scope() -> actix_web::Scope {
    web::scope("/feedback").service(actix_web::services![
        feedback_redirect,
        feedback_authorize,
        feedback_token
    ])
}

#[get("/login", name = "feedback_redirect")]
async fn feedback_redirect(
    config: web::Data<Config>,
    web::Query(mut query): web::Query<HashMap<String, String>>,
    session: actix_session::Session,
) -> Result<HttpResponse, actix_web::Error> {
    let hash = query
        .remove("hash")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing hash"))?;

    let hash = hex::decode(hash).map_err(actix_web::error::ErrorBadRequest)?;

    let id: i64 = query
        .get("id")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing id"))?
        .parse()
        .map_err(actix_web::error::ErrorBadRequest)?;

    let first_name = query
        .get("first_name")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing first_name"))?
        .to_owned();

    let auth_date: i64 = query
        .get("auth_date")
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing auth_date"))?
        .parse()
        .map_err(actix_web::error::ErrorBadRequest)?;

    let auth_date = chrono::Utc.timestamp_opt(auth_date, 0).unwrap();
    if auth_date + chrono::Duration::minutes(15) < chrono::Utc::now() {
        return Err(actix_web::error::ErrorBadRequest("data too old"));
    }

    let mut data: Vec<_> = query.into_iter().collect();
    data.sort_by(|(a, _), (b, _)| a.cmp(b));

    let data = data
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, value))
        .collect::<Vec<_>>()
        .join("\n");

    let token = Sha256::digest(&config.telegram_api_key);

    let mut mac = Hmac::<Sha256>::new_from_slice(&token)
        .expect("hmac could not be constructed with provided token");
    mac.update(data.as_bytes());

    mac.verify_slice(&hash)
        .map_err(actix_web::error::ErrorBadRequest)?;

    session.insert("telegram-login", (id, first_name))?;

    Ok(HttpResponse::Found()
        .insert_header(("Location", config.fider_oauth_url.clone()))
        .finish())
}

#[derive(Debug, Deserialize)]
struct FiderOAuthQuery {
    redirect_uri: String,
    state: String,
}

#[derive(Serialize, Deserialize)]
struct FiderCodeData {
    telegram_id: i64,
    telegram_first_name: String,
    exp: i64,
}

#[derive(Template)]
#[template(path = "signin.html")]
struct SignInTemplate<'a> {
    bot_username: &'a str,
    auth_url: &'a str,
}

#[post("/feedback/authorize", name = "feedback_authorize")]
async fn feedback_authorize(
    config: web::Data<Config>,
    session: actix_session::Session,
    web::Query(query): web::Query<FiderOAuthQuery>,
) -> Result<HttpResponse, actix_web::Error> {
    let login_data: (i64, String) = match session.get("telegram-login") {
        Ok(Some(id)) => id,
        _ => {
            let auth_url = format!("{}/feedback", config.public_endpoint);

            return SignInTemplate {
                bot_username: &config.telegram_bot_username,
                auth_url: &auth_url,
            }
            .render()
            .map(|body| {
                HttpResponse::Ok()
                    .insert_header(("content-type", "text/html"))
                    .body(body)
            })
            .map_err(actix_web::error::ErrorInternalServerError);
        }
    };

    if !query
        .redirect_uri
        .to_ascii_lowercase()
        .starts_with(&config.feedback_base.to_ascii_lowercase())
    {
        return Err(actix_web::error::ErrorBadRequest("redirect uri not valid"));
    }

    let code = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &FiderCodeData {
            telegram_id: login_data.0,
            telegram_first_name: login_data.1,
            exp: (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        },
        &jsonwebtoken::EncodingKey::from_secret(config.jwt_secret.as_bytes()),
    )
    .map_err(actix_web::error::ErrorInternalServerError)?;

    let mut location =
        url::Url::parse(&query.redirect_uri).map_err(actix_web::error::ErrorBadRequest)?;

    location
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &query.state);

    Ok(HttpResponse::Found()
        .insert_header(("Location", location.as_str()))
        .finish())
}

#[derive(Deserialize)]
struct FiderOAuthToken {
    code: String,
}

#[post("/feedback/token", name = "feedback_token")]
async fn feedback_token(
    config: web::Data<Config>,
    form: web::Form<FiderOAuthToken>,
    req: actix_web::HttpRequest,
) -> Result<HttpResponse, actix_web::Error> {
    let auth = req
        .headers()
        .get("authorization")
        .and_then(|auth| auth.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Basic "))
        .and_then(|auth| base64::decode(auth).ok())
        .and_then(|auth| String::from_utf8(auth).ok())
        .ok_or_else(|| actix_web::error::ErrorBadRequest("missing authorization"))?;

    match auth.split_once(':') {
        Some((username, password)) if username == "foxbot" && password == config.jwt_secret => (),
        _ => return Err(actix_web::error::ErrorUnauthorized("bad authorization")),
    }

    jsonwebtoken::decode::<FiderCodeData>(
        &form.code,
        &jsonwebtoken::DecodingKey::from_secret(config.jwt_secret.as_bytes()),
        &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
    )
    .map_err(actix_web::error::ErrorInternalServerError)?;

    Ok(HttpResponse::Ok()
        .append_header(("content-type", "application/json"))
        .body(
            serde_json::to_string(&serde_json::json!({
                "access_token": form.code,
                "token_type": "Bearer",
            }))
            .map_err(actix_web::error::ErrorInternalServerError)?,
        ))
}
