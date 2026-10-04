//! HTTP endpoints grouped by the resource they operate on.
mod chat;
mod conversations;
mod models;
mod response;
mod runs;

pub(crate) use response::error;

pub(crate) fn router() -> utoipa_axum::router::OpenApiRouter<crate::AppState> {
    use utoipa_axum::{router::OpenApiRouter, routes};
    #[derive(utoipa::OpenApi)]
    #[openapi(components(schemas(crate::api::RunAction)))]
    struct ApiDoc;
    use utoipa::OpenApi;
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(chat::chat))
        .routes(routes!(models::config))
        .routes(routes!(conversations::create, conversations::list))
        .routes(routes!(conversations::snapshot))
        .routes(routes!(conversations::receipt))
        .routes(routes!(runs::run))
        .routes(routes!(runs::control))
}
