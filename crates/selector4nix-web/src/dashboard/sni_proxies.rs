use std::net::IpAddr;
use std::sync::Arc;

use axum::Form;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use selector4nix_core::AppContext;
use selector4nix_core::AppError;
use selector4nix_core::application::usecase::sni_proxy::AddSniProxyCommand;
use selector4nix_core::domain::common::url::Url;
use serde::Deserialize;

use crate::WebAppError;
use crate::dashboard::substituters::respond_mutation;

#[derive(Debug, Deserialize)]
pub struct AddSniProxyForm {
    url: String,
    ip: String,
}

pub async fn post_add_sni_proxy(
    State(ctx): State<Arc<AppContext>>,
    headers: HeaderMap,
    Form(form): Form<AddSniProxyForm>,
) -> Result<Response, WebAppError> {
    let command = parse_add_command(form)?;
    let result = ctx.add_sni_proxy_usecase.run(command).await;
    respond_mutation(&ctx, &headers, result, Some("sni-proxy-added")).await
}

fn parse_add_command(form: AddSniProxyForm) -> Result<AddSniProxyCommand, AppError> {
    let substituter_url = Url::new(form.url.trim())?;
    let ip = form
        .ip
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| AppError::input("SNI proxy IP must be a valid IPv4 or IPv6 address"))?;
    Ok(AddSniProxyCommand {
        substituter_url,
        ip,
    })
}
