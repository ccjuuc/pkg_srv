use std::{env::current_dir, path::PathBuf};

use axum::{
    routing::{get, post},
    http::StatusCode,
    response::{Html, IntoResponse},
    extract::Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
mod svg_png;
use base64::engine::general_purpose::STANDARD;
use base64::engine::Engine;
mod image_util;
mod chromium_icon;

#[derive(Serialize, Deserialize)]
struct User {
    id: u64,
    name: String,
}

fn default_format() -> String {
    "png".to_string()
}

#[derive(Serialize, Deserialize)]
struct ConvertRequest {
    logo_name: String,
    logo_data: String,
    output_path: String,
    #[serde(default = "default_format")]
    format: String,
}

#[derive(Serialize, Deserialize)]
struct OemRequest {
    logo_name: String,
    logo_data: String,
    document_name: String,
    document_data: String,
}

#[derive(Serialize, Deserialize)]
struct CornerRequest {
    logo_name: String,
    logo_data: String,
    radius: String,
}

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt::init();

    // build our application with a route
    let app = Router::new()
        .route("/", get(root))
        .route("/convert_image", post(convert_image))
        .route("/oem_convert",post(oem_convert))
        .route("/add_rounded_corners", post(add_rounded_corners))
        .route("/users", post(create_user));

    // run our app with hyper, listening globally on port 3000
    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// handler that responds with the content of index.html
async fn root() -> impl IntoResponse {
    let html_content = include_str!("./templates/index.html");
    Html(html_content.to_string())
}

// handler that creates a user
async fn create_user(Json(payload): Json<User>) -> impl IntoResponse {
    let user = User {
        id: 1,
        name: payload.name,
    };

    (StatusCode::CREATED, Json(user))
}

// handler that converts images based on the format parameter
async fn convert_image(Json(payload): Json<ConvertRequest>) -> impl IntoResponse {
    let logo_path_buf = current_dir().unwrap().join(&payload.logo_name);
    let logo_path = logo_path_buf.to_str().unwrap();
    let logo_data = STANDARD.decode(&payload.logo_data).unwrap();
    std::fs::write(&logo_path, &logo_data).unwrap();
    let output_path = &payload.output_path;
    let format = &payload.format;
   
    let ret = match format.as_str() {
        "ICO" => {
            image_util::generate_chromium_ico(logo_path, output_path)
        },
        "ICON" => {
            chromium_icon::convert_svg_to_chromium_icon(logo_path, output_path)
        }
        "ICNS" => {
            image_util::generate_chromium_icns(logo_path, output_path)
        }
        "PNG" => {
            if logo_path.ends_with(".svg") {
                svg_png::convert_svg_to_png(logo_path, output_path)
            } else {
                "svg file is required for PNG conversion".to_string()
            }
        }
        _ => return (StatusCode::BAD_REQUEST, "Unsupported format").into_response(),
    };
    (StatusCode::OK, ret.clone()).into_response()
}

// handler for OEM conversion
async fn oem_convert(Json(payload): Json<OemRequest>) -> impl IntoResponse {
    if !payload.document_name.is_empty() && !payload.document_data.is_empty() {
        let document_path_buf = current_dir().unwrap().join(&payload.document_name);
        let document_path = document_path_buf.to_str().unwrap();
        let document_data = STANDARD.decode(&payload.document_data).unwrap();
        std::fs::write(document_path, &document_data).unwrap();
        image_util::generate_chromium_icns(document_path, "document.icns");
    }

    if !payload.logo_name.is_empty() && !payload.logo_data.is_empty() {
        let logo_path_buf = current_dir().unwrap().join(&payload.logo_name);
        let logo_path = logo_path_buf.to_str().unwrap();
        let logo_data = STANDARD.decode(&payload.logo_data).unwrap();
        std::fs::write(logo_path, &logo_data).unwrap();

        let format = payload.logo_name.split('.').last().unwrap_or("png");
    
        let mut fix_logo_path = PathBuf::from(logo_path);
        if format == "svg" {
            fix_logo_path.set_file_name("tmp.png");
            svg_png::convert_svg_to_png(logo_path, fix_logo_path.file_name().unwrap().to_str().unwrap());
            image_util::generate_chromium_ico(fix_logo_path.to_str().unwrap(), "chromium.ico");
            chromium_icon::convert_svg_to_chromium_icon(logo_path, "product.icon");
        }
    
        let sizes_and_names = vec![
            (256, vec!["product_logo_256.png"]),
            (128, vec!["product_logo_128.png"]),
            (64, vec!["product_logo_64.png"]),
            (48, vec!["product_logo_48.png"]),
            (32, vec!["product_logo_32.png"]),
            (24, vec!["product_logo_24.png"]),
            (16, vec!["product_logo_16.png"]),
        ];
    
        for (size, name) in sizes_and_names {
            for n in name {
                image_util::resize_image_with_scaler(fix_logo_path.to_str().unwrap(), Some(n), size, size);
            }
        }
    
        let sizes_draw_names = vec![
            (600, 188, vec!["Logo.png"]),
            (176, 24, vec!["SmallLogo.png"])
        ];
    
        for (canvas_size, logo_size, name) in sizes_draw_names {
            for n in name {
                image_util::generate_chromium_logo(fix_logo_path.to_str().unwrap(), n, canvas_size, logo_size);
            }
        }
    
        image_util::generate_chromium_icns(fix_logo_path.to_str().unwrap(), "app.icns");
        
        image_util::generate_grayscale_image(fix_logo_path.to_str().unwrap(), "product_logo_22_mono.png",22);
    }
    
    (StatusCode::OK, "OEM images created successfully").into_response()
}

// handler for adding rounded corners to an image
async fn add_rounded_corners(Json(payload): Json<CornerRequest>) -> impl IntoResponse {
    let logo_path_buf = current_dir().unwrap().join(&payload.logo_name);
    let logo_path = logo_path_buf.to_str().unwrap();
    let logo_data = STANDARD.decode(&payload.logo_data).unwrap();
    std::fs::write(&logo_path, &logo_data).unwrap();
    let radius = &payload.radius;
    let outpath = image_util::apply_rounded_corners(logo_path, radius);
    (StatusCode::OK, outpath).into_response()
}