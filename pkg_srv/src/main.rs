use axum::{
    routing::{get, post},
    response::{Html, IntoResponse}, Router,
};

use tower_http::cors::{CorsLayer, Any};
use tokio::net::TcpListener;

mod pkg_tool;
mod oem_tool;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // initialize tracing
    tracing_subscriber::fmt::init();

    let cors = CorsLayer::new()
    .allow_origin(Any)
    .allow_methods(Any)
    .allow_headers(Any);
    
    let db_pool = pkg_tool::pkg_build::init_db().await?;

    // build our application with a route
    let app = Router::new()
        .route("/", get(pkgbuild))
        .route("/oem", get(oempage))
        .route("/convert_image", post(oem_tool::oem_srv::convert_image))
        .route("/oem_convert",post(oem_tool::oem_srv::oem_convert))
        .route("/add_rounded_corners", post(oem_tool::oem_srv::add_rounded_corners))
        .route("/server_list", get(pkg_tool::pkg_build::server_list))
        .route("/oem_list", get(pkg_tool::pkg_build::oem_list))
        .route("/task_list", get(pkg_tool::pkg_build::task_list))
        .route("/add_task", post(pkg_tool::pkg_build::add_task))
        .route("/update_task", post(pkg_tool::pkg_build::update_task))
        .route("/build_package", post(pkg_tool::pkg_build::build_package))
        .route("/download/*file_path", get(pkg_tool::pkg_build::download_installer))
        .route("/delete_task", post(pkg_tool::pkg_build::delete_task))
        .layer(cors)
        .with_state(db_pool);

    // run our app with hyper, listening globally on port 3000
    let listener = TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// handler that responds with the content of index.html
async fn oempage() -> impl IntoResponse {
    let html_content = include_str!("./templates/index.html");
    Html(html_content.to_string())
}

// handler that responds with the content of pkgbuild.html
async fn pkgbuild() -> impl IntoResponse {
    let html_content = include_str!("./templates/pkgbuild.html");
    Html(html_content.to_string())
}