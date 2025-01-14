use std::env::current_dir;
use std::path::PathBuf;

use axum::{
  extract::Json,
  http::StatusCode,
  response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use base64::engine::general_purpose::STANDARD;
use base64::engine::Engine;
use crate::oem_tool::chromium_icon;
use crate::oem_tool::image_util;
use crate::oem_tool::svg_png;

fn default_format() -> String {
  "png".to_string()
}

#[derive(Serialize, Deserialize)]
pub struct ConvertRequest {
  logo_name: String,
  logo_data: String,
  output_path: String,
  #[serde(default = "default_format")]
  format: String,
}

#[derive(Serialize, Deserialize)]
pub struct OemRequest {
  logo_name: String,
  logo_data: String,
  document_name: String,
  document_data: String,
}

#[derive(Serialize, Deserialize)]
pub struct CornerRequest {
  logo_name: String,
  logo_data: String,
  radius: String,
}
// handler that converts images based on the format parameter
pub async fn convert_image(Json(payload): Json<ConvertRequest>) -> impl IntoResponse {
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
          image_util::generate_chromium_icns(logo_path, output_path, true)
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
pub async fn oem_convert(Json(payload): Json<OemRequest>) -> impl IntoResponse {
  let logo_dir  = current_dir().unwrap().join("oem_logo");
  if !logo_dir.exists() {
      std::fs::create_dir(&logo_dir).unwrap();
  }
  if !payload.logo_name.is_empty() && !payload.logo_data.is_empty() {
      let logo_path_buf = logo_dir.join(&payload.logo_name);
      let logo_path = logo_path_buf.to_str().unwrap();
      let logo_data = STANDARD.decode(&payload.logo_data).unwrap();
      std::fs::write(logo_path, &logo_data).unwrap();

      let format = payload.logo_name.split('.').last().unwrap_or("png");
    
      let mut fix_logo_path = PathBuf::from(logo_path);
      if format == "svg" {
          fix_logo_path.set_file_name("tmp.png");
          svg_png::convert_svg_to_png(logo_path, fix_logo_path.file_name().unwrap().to_str().unwrap());
          chromium_icon::convert_svg_to_chromium_icon(logo_path, "product.icon");
      }

      image_util::generate_chromium_ico(fix_logo_path.to_str().unwrap(), "chromium.ico");
  
      let sizes_and_names = vec![
          (256, vec!["product_logo_256.png"]),
          (192, vec!["product_logo_192.png"]),
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
  
      image_util::generate_chromium_icns(fix_logo_path.to_str().unwrap(), "app.icns", true);
      
      image_util::generate_grayscale_image(fix_logo_path.to_str().unwrap(), "product_logo_22_mono.png",22);
  }

  if !payload.document_name.is_empty() && !payload.document_data.is_empty() {
    let document_path_buf = logo_dir.join(&payload.document_name);
    let document_path = document_path_buf.to_str().unwrap();
    let document_data = STANDARD.decode(&payload.document_data).unwrap();
    std::fs::write(document_path, &document_data).unwrap();
    image_util::generate_chromium_document_icns(document_path, "document.icns");
  }
  
  (StatusCode::OK, "OEM images created successfully").into_response()
}

// handler for adding rounded corners to an image
pub async fn add_rounded_corners(Json(payload): Json<CornerRequest>) -> impl IntoResponse {
  let logo_path_buf = current_dir().unwrap().join(&payload.logo_name);
  let logo_path = logo_path_buf.to_str().unwrap();
  let logo_data = STANDARD.decode(&payload.logo_data).unwrap();
  std::fs::write(&logo_path, &logo_data).unwrap();
  let radius = &payload.radius;
  let outpath = image_util::apply_rounded_corners(logo_path, radius);
  (StatusCode::OK, outpath).into_response()
}