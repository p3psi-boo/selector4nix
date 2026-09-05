use std::sync::Arc;

use axum::Form;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use selector4nix_core::AppContext;
use selector4nix_core::application::usecase::substituter::AddSubstituterCommand;
use selector4nix_core::domain::common::url::Url;
use selector4nix_core::domain::substituter::model::Priority;
use selector4nix_core::{AppError, AppErrorKind};
use serde::Deserialize;

use crate::WebAppError;
use crate::dashboard::overview::render_overview_page;

#[derive(Debug, Deserialize)]
pub struct AddSubstituterForm {
    url: String,
    priority: Option<String>,
    storage_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SubstituterUrlForm {
    url: String,
}

pub async fn post_add_substituter(
    State(ctx): State<Arc<AppContext>>,
    headers: HeaderMap,
    Form(form): Form<AddSubstituterForm>,
) -> Result<Response, WebAppError> {
    let command = parse_add_command(form)?;
    let result = ctx.add_substituter_usecase.run(command).await;
    respond_mutation(&ctx, &headers, result, Some("substituter-added")).await
}

pub async fn post_enable_substituter(
    State(ctx): State<Arc<AppContext>>,
    headers: HeaderMap,
    Form(form): Form<SubstituterUrlForm>,
) -> Result<Response, WebAppError> {
    let url = Url::new(&form.url)?;
    let result = ctx.enable_substituter_usecase.run(url).await;
    respond_mutation(&ctx, &headers, result, None).await
}

pub async fn post_disable_substituter(
    State(ctx): State<Arc<AppContext>>,
    headers: HeaderMap,
    Form(form): Form<SubstituterUrlForm>,
) -> Result<Response, WebAppError> {
    let url = Url::new(&form.url)?;
    let result = ctx.disable_substituter_usecase.run(url).await;
    respond_mutation(&ctx, &headers, result, None).await
}

fn parse_add_command(form: AddSubstituterForm) -> Result<AddSubstituterCommand, AppError> {
    let url = Url::new(form.url.trim())?;
    let priority = parse_optional_priority(form.priority.as_deref())?;
    let storage_url = parse_optional_url(form.storage_url.as_deref())?;
    Ok(AddSubstituterCommand {
        url,
        priority,
        storage_url,
    })
}

fn parse_optional_url(value: Option<&str>) -> Result<Option<Url>, AppError> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        Some(value) => Ok(Some(Url::new(value)?)),
        None => Ok(None),
    }
}

fn parse_optional_priority(value: Option<&str>) -> Result<Priority, AppError> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        Some(value) => {
            let parsed = value
                .parse::<u32>()
                .map_err(|_| AppError::input("priority must be a positive integer"))?;
            Ok(Priority::new(parsed)?)
        }
        None => Ok(Priority::new(40).expect("default substituter priority is valid")),
    }
}

pub(crate) async fn respond_mutation(
    ctx: &AppContext,
    headers: &HeaderMap,
    result: Result<(), AppError>,
    success_trigger: Option<&'static str>,
) -> Result<Response, WebAppError> {
    let is_htmx = headers.contains_key("hx-request");
    match result {
        Ok(()) if is_htmx => {
            let mut response = render_overview_page(ctx, headers, None)
                .await
                .into_response();
            if let Some(trigger) = success_trigger {
                response.headers_mut().insert(
                    "HX-Trigger",
                    trigger.parse().expect("trigger is a valid header value"),
                );
            }
            Ok(response)
        }
        Ok(()) => Ok(Redirect::to("/dashboard/").into_response()),
        Err(err)
            if is_htmx
                && matches!(
                    err.kind(),
                    AppErrorKind::Input | AppErrorKind::Rule | AppErrorKind::NotFound
                ) =>
        {
            Ok(render_overview_page(ctx, headers, Some(err.to_string()))
                .await
                .into_response())
        }
        Err(err) => Err(err.into()),
    }
}
