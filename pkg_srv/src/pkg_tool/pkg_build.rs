use std::collections::HashMap;
use std::path::Path;
use std::process::Output;
use std::{env, vec};
use std::{path::PathBuf, process::Command};

use axum::http::header::{self};
use axum::response::Response;
use axum::{
    extract::{Json, Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use crypto::{digest::Digest, md5::Md5};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{Executor, Row, SqlitePool};
use tokio::fs;
use tokio::{fs::File, io::AsyncReadExt, task};
use toml;
use regex::Regex;
use walkdir::WalkDir;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "macos")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const IDE: &str = "xcode";
    pub const INSTALLER_PROJECT: &str = "chrome/installer/mac";
}

#[cfg(target_os = "linux")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-c"];
    pub const IDE: &str = "";
    pub const INSTALLER_PROJECT: &str = "chrome/installer/linux:stable";
}

#[cfg(windows)]
mod os {
    pub const SHELL: [&str; 2] = ["cmd.exe", "/c"];
    pub const IDE: &str = "vs2022";
    pub const INSTALLER_PROJECT: &str = "installer_with_sign";
}

#[derive(Serialize, Debug, Default)]
struct Task {
    id: i64,
    start_time: String,
    end_time: String,
    branch_name: String,
    oem_name: String,
    commit_id: String,
    is_signed: bool,
    is_increment: bool,
    storage_path: String,
    installer: String,
    state: String,
    server: String,
}

const TASKLIST_QUERY: &str = r#"
  SELECT id, start_time, branch_name, end_time, oem_name, commit_id, is_signed, is_increment, storage_path,installer, state, server
  FROM pkg
  ORDER BY id DESC
"#;

const ADD_TASK: &str = r#"
INSERT INTO pkg (start_time, branch_name, oem_name, commit_id, is_increment, is_signed, server)
VALUES (datetime('now', 'localtime'), ?, ?, ?, ?, ?, ?)
RETURNING id
"#;

const UPDATE_TASK: &str = r#"
UPDATE pkg
SET end_time = ?, storage_path = ?, installer = ?, state = ?
WHERE id = ?
"#;

#[derive(Deserialize, Clone)]
pub struct PkgBuildRequest {
    branch: String,
    commit_id: Option<String>,
    is_x64: bool,
    platform: String,
    is_increment: bool,
    is_signed: bool,
    server: String,
    oem_name: String,
    password: String,
}


#[derive(Deserialize)]
pub struct UpdateTaskRequest {
    id: i64,
    end_time: String,
    storage_path: String,
    installer: String,
    state: String,
}

#[derive(Deserialize)]
pub struct AddTaskRequest {
    branch: String,
    oem_name: Option<String>,
    commit_id: String,
    is_increment: bool,
    is_signed: bool,
    server: String,
}

#[derive(Deserialize)]
pub struct DeleteTaskRequest {
    task_id: i64,
}

fn print_info(msg: &[u8]) {
    let stdout_str = String::from_utf8_lossy(msg);
    let stdout_trimmed = match stdout_str.find('[') {
        Some(index) => &stdout_str[..index],
        None => &stdout_str,
    };
    println!("{}", stdout_trimmed);
}

pub async fn server_list() -> impl IntoResponse {
    let mut file = File::open("config.toml").await.unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();

    let config: toml::Value = toml::from_str(&contents).unwrap();
    let data  = serde_json::to_string(&config["server"]).unwrap();

    (StatusCode::OK, data)
}

pub async fn oem_list() -> impl IntoResponse {
    let mut file = File::open("config.toml").await.unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();

    let config: toml::Value = toml::from_str(&contents).unwrap();
    let data = serde_json::to_string(&config["oem"]).unwrap();

    (StatusCode::OK, data)
}

pub async fn delete_task(State(db_pool): State<SqlitePool>, Json(payload): Json<DeleteTaskRequest>) -> impl IntoResponse {
    let task_id = payload.task_id;
    
    let record = match sqlx::query("SELECT * FROM pkg WHERE id = ?")
        .bind(task_id)
        .fetch_one(&db_pool)
        .await
    {
        Ok(record) => record,
        Err(_) => return (StatusCode::NOT_FOUND, "Task not found".into()),
    };

    let storage_path = record.get::<String, _>("storage_path");
    if !storage_path.is_empty() && Path::new(&storage_path).exists() {
        let _ = fs::remove_dir_all(&storage_path).await;
    }

    match sqlx::query("DELETE FROM pkg WHERE id = ?")
    .bind(task_id)
    .execute(&db_pool)
    .await
    {
        Ok(_) => {}
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete task".into()),
    }

    

    (StatusCode::OK, "Task deleted")
}

pub async fn task_list(State(db_pool): State<SqlitePool>) -> impl IntoResponse {
    let records: Vec<sqlx::sqlite::SqliteRow> = sqlx::query(TASKLIST_QUERY)
        .fetch_all(&db_pool)
        .await
        .expect("failed to fetch records");

    let tasks: Vec<Task> = records
        .iter()
        .map(|row| Task {
            id: row.get::<i64, _>("id"),
            branch_name: row.get::<String, _>("branch_name"),
            start_time: row.get::<String, _>("start_time"),
            end_time: row.get("end_time"),
            is_signed: row.get("is_signed"),
            storage_path: row.get("storage_path"),
            installer: row.get("installer"),
            state: row.get("state"),
            commit_id: row.get("commit_id"),
            is_increment: row.get("is_increment"),
            oem_name: row.get("oem_name"),
            server: row.get("server"),
        })
        .collect();
    let json_result = serde_json::json!({"tasks": tasks});
    axum::Json(json_result)
}
pub async fn download_installer(AxumPath(file_path): AxumPath<String>) -> impl IntoResponse {
    let mut file = File::open("config.toml").await.unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();

    let config: toml::Value = toml::from_str(&contents).unwrap();
    if let Some(backup_path) = config.get("backup_path") {
        if let Some(path) = backup_path.get(std::env::consts::OS) {
            let path = path.as_str().unwrap_or_default();
            let download_file = Path::new(path).join(&file_path);
            if download_file.exists() {
                let file_name = Path::new(&download_file)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string();
                let file = fs::read(&download_file).await.unwrap();

                return Response::builder()
                    .header(header::CONTENT_TYPE, "application/octet-stream")
                    .header(
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}\"", file_name),
                    )
                    .body(axum::body::Body::from(file))
                    .unwrap();
            }
        }
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(axum::body::Body::from("File not found"))
        .unwrap()
}

pub async fn build_package(
    State(db_pool): State<SqlitePool>,
    Json(payload): Json<PkgBuildRequest>,
) -> impl IntoResponse {
    let current_dir = std::env::current_dir().unwrap();
    println!("Current directory: {:?}", current_dir);
    let mut file = File::open("config.toml").await.unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();

    let config: toml::Value = toml::from_str(&contents).unwrap();

    let src_path = config["src"]["path"].as_str().unwrap();
    println!("Source code path: {}", src_path);

    println!("Branch: {}", payload.branch);
    println!("Commit ID: {:?}", payload.commit_id);
    println!("Platform: {}", payload.platform);
    println!("Is x64: {}", payload.is_x64);

    let payload_clone = payload.clone();
    let db_pool_clone = db_pool.clone();
    task::spawn(async move {
        if let Err(e) = do_build(&payload_clone, &db_pool_clone).await {
            let task_id = e.downcast_ref::<i64>().unwrap_or(&-1);
            update_task_state(
                "",
                *task_id,
                "",
                "",
                "",
                "failed",
                &db_pool_clone,
            )
            .await;
        }
    });

    (StatusCode::OK, "Package build started")
}

fn update_code(src_path: &str, branch: &str, commit_id: &Option<String>) {
    Command::new("git")
        .arg("stash")
        .current_dir(&src_path)
        .output()
        .expect("failed to execute git stash");

    if let Some(commit_id) = commit_id {
        Command::new("git")
            .arg("checkout")
            .arg(commit_id)
            .current_dir(&src_path)
            .output()
            .expect("failed to execute git checkout");
        return;
    }

    Command::new("git")
        .arg("checkout")
        .arg(branch)
        .current_dir(&src_path)
        .output()
        .expect("failed to execute git checkout");

    Command::new("git")
        .arg("pull")
        .current_dir(&src_path)
        .output()
        .expect("failed to execute git pull");
}

pub async fn update_task(
    State(db_pool): State<SqlitePool>,
    Json(payload): Json<UpdateTaskRequest>,
) -> impl IntoResponse {
    update_task_state(
        "local",
        payload.id,
        &payload.end_time,
        &payload.storage_path,
        &payload.installer,
        &payload.state,
        &db_pool,
    ).await;
    (StatusCode::OK, "Task updated")
}

pub async fn add_task(
    State(db_pool): State<SqlitePool>,
    Json(payload): Json<AddTaskRequest>,
) -> impl IntoResponse {
    let task_id = add_task_state(
        &payload.server,
        &payload.branch,
        payload.oem_name.as_deref().unwrap_or(""),
        &payload.commit_id,
        payload.is_increment,
        payload.is_signed,
        &db_pool,
    ).await;
    match task_id {
        Ok(id) => (StatusCode::OK, id.to_string()),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Failed to add task".to_string()),  
    }
}

async fn add_task_state(
    server: &str,
    branch: &str,
    oem_name: &str,
    commit_id: &str,
    is_increment: bool,
    is_signed: bool,
    db_pool: &SqlitePool,
) -> Result<i64, sqlx::Error> {
    let mut task_id: i64 = -1;
    if !db_pool.is_closed() {
        task_id = sqlx::query_scalar(ADD_TASK)
        .bind(branch)
        .bind(oem_name)
        .bind(commit_id)
        .bind(is_increment)
        .bind(is_signed)
        .bind(server)
        .fetch_one(db_pool)
        .await
        .expect("failed to add task");
    } else {
        let client = Client::new();
        let response = client
            .post(format!("http://{}/add_task", server))
            .json(&serde_json::json!({
                "branch": branch,
                "oem_name": oem_name,
                "commit_id": commit_id,
            }))
            .send()
            .await
            .expect("failed to update_task");
        if response.status() == 200 {
            task_id = response.text().await.unwrap().parse().unwrap();
        }
    }
    Ok(task_id)
}
async fn update_task_state(
    server: &str,
    task_id: i64,
    end_time: &str,
    store_path: &str,
    installer: &str,
    state: &str,
    db_pool: &SqlitePool,
) {
    if !db_pool.is_closed() {
        sqlx::query(UPDATE_TASK)
            .bind(&end_time)
            .bind(&store_path)
            .bind(&installer)
            .bind(&state)
            .bind(&task_id)
            .execute(db_pool)
            .await
            .expect("failed to update task");
    } else {
        let client = Client::new();
        let _response = client
            .post(format!("http://{}/update_task", server))
            .json(&serde_json::json!({
                "task_id": task_id,
                "end_time": end_time,
                "store_path": store_path,
                "installer": installer,
                "state": state,
            }))
            .send()
            .await
            .expect("failed to update_task");
    }
}

async fn copy_debug_files(data_dir: &Path, backup_dir: &Path, oem: &str) -> anyhow::Result<()> {
    if !backup_dir.exists() {
        fs::create_dir_all(&backup_dir).await.unwrap();
    }
    for entry in WalkDir::new(data_dir).max_depth(1).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let file_name_lower = file_name.to_lowercase();
            if file_name.ends_with(".pdb") || file_name.ends_with(".dbg") || file_name.ends_with(".debug") {
                if file_name_lower.contains(oem) {
                    fs::copy(entry.path(), backup_dir.join(file_name)).await?;
                }
            }
        }
    }
    Ok(())
}

async fn backup(src_path: &str, out_dir: &str, out_dir_64: &str, config: toml::Value, server_addr: &str, task_id: i64, db_pool: &SqlitePool, oem: &str, installer: HashMap<&str,&str>,installer_64: HashMap<&str,&str>) -> anyhow::Result<()> {
    if let Some(backup_path) = config.get("backup_path") {
        if let Some(path) = backup_path.get(std::env::consts::OS) {
            let backup_dir = path.as_str().unwrap_or_default();
            if !backup_dir.is_empty() {
                let date_subfolder = chrono::Local::now().format("%Y-%m-%d-%H-%M").to_string();
                let date_dir = Path::new(backup_dir).join(&date_subfolder);
                fs::create_dir_all(&date_dir).await.unwrap();
                let end_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

                let dst= Path::new(src_path).join(out_dir);
                let backup_subfolder = date_dir.join(oem);

                for (installer, _) in installer.iter() {
                    let _ = fs::copy(&dst.join(installer), date_dir.join(installer)).await;
                }
                let dst_64 = Path::new(src_path).join(out_dir_64);
                let backup_subfolder_64 = date_dir.join(format!("{}_x64", oem));

                for (installer, _) in installer_64.iter() {
                    let _ = fs::copy(&dst_64.join(installer), date_dir.join(installer)).await;
                }

                let mut installer = installer.clone();
                installer.extend(installer_64);
                let installer_vec: Vec<(String, String)> = installer.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                let installer_json = serde_json::to_string(&installer_vec).unwrap();

                update_task_state(
                    &server_addr,
                    task_id,
                    &end_time,
                    date_dir.to_str().unwrap(),
                    &installer_json,
                    "success",
                    db_pool,
                )
                .await;
                
                if !oem.is_empty() {
                    if !out_dir.is_empty() {
                        copy_debug_files(&dst, &backup_subfolder, oem).await?;
                    }
                    if !out_dir_64.is_empty() {
                        copy_debug_files(&dst_64, &backup_subfolder_64, oem).await?;
                    }
                }
            }
        }
    }
    Ok(())
}
async fn calc_installer_md5(pkg_path: &str, extension: &str) -> (String, String) {
    let mut installer_file = pkg_path.to_string();
    let mut md5 = String::new();
    if Path::new(pkg_path).is_dir() {
        for entry in WalkDir::new(pkg_path).max_depth(1).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    let version_regex = Regex::new(r"\d+\.\d+\.\d+\.\d+").unwrap();
                    if version_regex.is_match(file_name) {
                        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                            if extension == ext {
                                installer_file = path.to_string_lossy().to_string();
                            }
                        }
                    }
                }
            }
        }
    }

    if Path::new(&installer_file).exists() {
        let mut file = File::open(&installer_file).await.unwrap();
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).await.unwrap();
        let mut hasher = Md5::new();
        hasher.input(&buffer);
        md5 = hasher.result_str();
    }
    (
        Path::new(&installer_file)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string(),
        md5
    )
}


async fn build_installer(src_path: &str, out_dir: &str, _config: toml::Value, server_addr: &str, task_id: i64, _payload: &PkgBuildRequest, db_pool: &SqlitePool, _x64: bool) -> anyhow::Result<()> {
        update_task_state(
            &server_addr,
            task_id,
            "",
            "",
            "",
            "build installer",
            db_pool,
        )
        .await;
        let mini_installer_command = format!("ninja -C {} {}", out_dir, os::INSTALLER_PROJECT);
        let mut mini_installer_output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .arg(&mini_installer_command)
            .current_dir(&src_path)
            .spawn()
            .expect("failed to execute build installer command");
        if !mini_installer_output.wait().unwrap().success() {
            println!("mini_installer failed");
            return Err(anyhow::anyhow!(format!("task_id: {}, command: {}", task_id, mini_installer_command)));
        }

    Ok(())
}

async fn build_project(src_path: &str, out_dir: &str, _config: toml::Value, server_addr: &str, task_id: i64, payload: &PkgBuildRequest, db_pool: &SqlitePool, _x64: bool) -> anyhow::Result<()> {
    if !payload.is_increment {
        fs::remove_dir_all(Path::new(src_path).join(out_dir)).await?;
    }
    update_task_state(
        &server_addr,
        task_id,
        "",
        "",
        "",
        "build pre_build",
        db_pool,
    )
    .await;
    let pre_build_command = format!("ninja -C {} pre_build", out_dir);
    let mut pre_build_output = Command::new(os::SHELL[0])
        .arg(os::SHELL[1])
        .arg(&pre_build_command)
        .current_dir(&src_path)
        .spawn()
        .expect("failed to execute pre_build command");

    if !pre_build_output.wait().unwrap().success() {
        println!("pre_build failed");
        return Err(anyhow::anyhow!(format!("task_id: {}, command: {}", task_id, pre_build_command)));
    }

    update_task_state(&server_addr, task_id, "", "", "", "build base", db_pool).await;
    let base_build_command = format!("ninja -C {} base", out_dir);
    let mut base_build_output = Command::new(os::SHELL[0])
        .arg(os::SHELL[1])
        .arg(&base_build_command)
        .current_dir(&src_path)
        .spawn()
        .expect("failed to execute base build command");
    if !base_build_output.wait().unwrap().success() {
        println!("base_build failed");
        return Err(anyhow::anyhow!(format!("task_id: {}, command: {}", task_id, base_build_command)));
    }

    update_task_state(
        &server_addr,
        task_id,
        "",
        "",
        "",
        "build chrome",
        db_pool,
    )
    .await;
    let chrome_build_command = format!("ninja -C {} chrome", out_dir);
    let mut chrome_build_output = Command::new(os::SHELL[0])
        .arg(os::SHELL[1])
        .arg(&chrome_build_command)
        .current_dir(&src_path)
        .spawn()
        .expect("failed to execute chrome build command");
    
    if !chrome_build_output.wait().unwrap().success() {
        println!("chrome_build failed");
        return Err(anyhow::anyhow!(format!("task_id: {}, command: {}", task_id, chrome_build_command)));
    }

    Ok(())
}

async fn make_project(src_path: &str, out_dir: &str, config: toml::Value, server_addr: &str, task_id: i64, payload: &PkgBuildRequest, db_pool: &SqlitePool, x64: bool) -> anyhow::Result<()> {
    let mut args = vec![];

    if let Some(custom_args) = config.get("gn_default_args") {
        if let Some(spec_args) = custom_args.get(std::env::consts::OS) {
            for arg in spec_args.as_array().unwrap() {
                if !arg.as_str().unwrap().is_empty() {
                    args.push(arg.as_str().unwrap());
                }
            }
        }
    }

    let target_cpu = if x64 { "target_cpu=\\\"x64\\\"" } else { "" };
    if !target_cpu.is_empty() {
        args.push(target_cpu);
    }
    
    #[warn(unused_assignments)]
    let mut oem_arg = "".to_string();
    if !payload.oem_name.is_empty() {
        let prefix = payload.oem_name.split('=').nth(0).unwrap_or("current_xn_brand");
        let oem = payload.oem_name.split('=').nth(1).unwrap_or("normal");
        oem_arg = format!("{}=\\\"{}\\\"", prefix, oem);
        args.push(&oem_arg);
    }

    println!("oem_arg: {}", oem_arg);

    if payload.password.is_empty() {
        args.push(&payload.password);
    }

    update_task_state(
        &server_addr,
        task_id,
        "",
        "",
        "",
        "gen project",
        db_pool,
    )
    .await;

    let ide_args = if os::IDE.is_empty() {
        ""
    } else {
        &format!("--ide={}", os::IDE)
    };

    let gn_args = &[
        "gn",
        "gen",
        out_dir,
        &format!("--args=\"{}\"", args.join(" ")),
        ide_args,
    ];

    let gn_output: Output;
    #[cfg(target_os = "windows")]
    {
        gn_output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .raw_arg(gn_args.join(" "))
            .current_dir(&src_path)
            .output()
            .expect("failed to execute gn command");
    }
    #[cfg(not(target_os = "windows"))]
    {
        gn_output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .arg(gn_args.join(" "))
            .current_dir(&src_path)
            .output()
            .expect("failed to execute gn command");
    }
    if gn_output.status.success() {
        print_info(&gn_output.stdout);
    } else {
        print_info(&gn_output.stdout);
        return Err(anyhow::anyhow!(task_id));
    }
    Ok(())
}
async fn do_build(payload: &PkgBuildRequest, db_pool: &SqlitePool) -> anyhow::Result<()> {
    let server_addr = payload.server.clone();
    let oem = payload.oem_name.split('=').nth(1).unwrap_or("default").to_string();
    let task_id = add_task_state(
        &server_addr,
        &payload.branch,
        &oem,
        payload.commit_id.as_deref().unwrap_or(""),
        payload.is_increment,
        payload.is_signed,
        db_pool
    ).await.unwrap_or(-1);

    let _task = Task {
        id: task_id,
        branch_name: payload.branch.clone(),
        start_time: "".to_string(),
        end_time: "".to_string(),
        is_signed: false,
        storage_path: "".to_string(),
        installer: "".to_string(),
        state: "".to_string(),
        commit_id: payload.commit_id.clone().unwrap_or_default(),
        is_increment: false,
        oem_name: payload.oem_name.clone(),
        server: payload.server.clone(),
    };

    update_task_state(&server_addr, task_id, "", "", "", "clean...", db_pool).await;

    let mut file = File::open("config.toml")
        .await
        .expect("failed to open config file");

    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();
    let config: toml::Value = toml::from_str(&contents).unwrap();
    use std::path::Path;

    let src_path = {
            if let Some(custom_args) = config.get("src") {
                if let Some(src) = custom_args.get(std::env::consts::OS) {
                    src.as_str().unwrap()
                } else {
                    ""
                }
            } else {
                ""
            }
        };
    println!("Source code path: {}", src_path);

    update_task_state(
        &server_addr,
        task_id,
        "",
        "",
        "",
        "checkout...",
        db_pool,
    )
    .await;
    update_code(src_path, &payload.branch, &payload.commit_id);

    if let Some(clean) = config.get("clean") {
        if let Some(path) = clean.get("path") {
            for p in path.as_array().unwrap() {
                let path = Path::new(src_path).join(p.as_str().unwrap());
                if Path::new(&path).exists() {
                    if path.is_file() {
                        let _ = std::fs::remove_file(&path);
                    } else {
                        let _ = std::fs::remove_dir_all(&path);
                    }
                }
            }
        }
    }

    if cfg!(target_os = "macos") {
        let out_dir_64 = format!("out/{}_x64", oem);
        make_project(src_path, &out_dir_64, config.clone(),&server_addr,task_id,payload, db_pool, false).await?;
        build_project(src_path, &out_dir_64, config.clone(), &server_addr, task_id, payload, db_pool, true).await?;
        let out_dir = format!("out/{}", oem);
        make_project(src_path, &out_dir, config.clone(),&server_addr,task_id,payload, db_pool, true).await?;
        build_project(src_path, &out_dir, config.clone(), &server_addr, task_id, payload, db_pool, false).await?;
        let pkg = format!("{}/{} Browser.app", out_dir, payload.oem_name);
        let pkg_64 = format!("{}/{} Browser.app", out_dir_64, payload.oem_name);
        let pkg_target_dir = format!("out/release_universalizer_{}/", payload.oem_name);
        if Path::new(&pkg_target_dir).exists() {
            let _ = fs::remove_dir_all(&pkg_target_dir).await;
        }
        fs::create_dir_all(&pkg_target_dir).await?;
        let pkg_target = format!("{}/{} Browser.app", pkg_target_dir, payload.oem_name);
        let universalizer_command = format!("python3 chrome/installer/mac/universalizer.py {} {} {}", pkg, pkg_64, pkg_target);
        let mut universalizer_output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .arg(&universalizer_command)
            .current_dir(&src_path)
            .spawn()
            .expect("failed to execute universalizer command");
        if !universalizer_output.wait().unwrap().success() {
            println!("universalizer failed");
            return Err(anyhow::anyhow!(task_id));
        }
        //压缩pkg_target
        let pkg_target_zip = format!("{}.zip", pkg_target);
        let zip_command = format!("zip -r -j {} {}", pkg_target_zip, pkg_target);
        let mut zip_output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .arg(&zip_command)
            .current_dir(&src_path)
            .spawn()
            .expect("failed to execute zip command");
        if !zip_output.wait().unwrap().success() {
            println!("zip failed");
            return Err(anyhow::anyhow!(task_id));
        }

        let sign_command = format!("sign_client {} {}", pkg_target_zip, payload.oem_name);
        let mut sign_output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .arg(&sign_command)
            .current_dir(&src_path)
            .spawn()
            .expect("failed to execute sign command");

        if !sign_output.wait().unwrap().success() {
            println!("sign failed");
            return Err(anyhow::anyhow!(task_id));
        }

        let (installer, md5) = calc_installer_md5(&pkg_target_zip, "zip").await;
        let mut installer_map = HashMap::new();
        installer_map.insert(installer.as_str(), md5.as_str());
        backup(src_path, out_dir.as_str(), out_dir_64.as_str(), config.clone(), &server_addr, task_id, db_pool, &oem, installer_map, HashMap::new()).await?;
    }
    else {
        let out_dir: String = format!("out/{}{}", oem, if payload.is_x64 { "_x64" } else { "" });
        let dst = Path::new(src_path).join(&out_dir);
        make_project(src_path, &out_dir, config.clone(),&server_addr,task_id,payload, db_pool, payload.is_x64).await?;
        build_project(src_path, &out_dir, config.clone(), &server_addr, task_id, payload, db_pool, payload.is_x64).await?;
        build_installer(src_path, &out_dir, config.clone(), &server_addr, task_id, payload, db_pool, payload.is_x64).await?;
        if cfg!(target_os = "linux") {
            let (installer_deb, md5_deb) = calc_installer_md5(dst.to_str().unwrap(), "deb").await;
            let (installer_rpm, md5_rpm) = calc_installer_md5(dst.to_str().unwrap(), "rpm").await;
            let pkg_target_deb_zip = format!("{}.zip", installer_deb);
            let pkg_target_rpm_zip = format!("{}.zip", installer_rpm);
            let zip_command_deb = format!("zip -r -j {} {}", pkg_target_deb_zip, installer_deb);
            let mut zip_output = Command::new(os::SHELL[0])
                .arg(os::SHELL[1])
                .arg(&zip_command_deb)
                .current_dir(&src_path)
                .spawn()
                .expect("failed to execute zip command");
            if !zip_output.wait().unwrap().success() {
                println!("zip failed");
                return Err(anyhow::anyhow!(task_id));
            }
            let zip_command_rpm = format!("zip -r -j {} {}", pkg_target_rpm_zip, installer_rpm);
            let mut zip_output = Command::new(os::SHELL[0])
                .arg(os::SHELL[1])
                .arg(&zip_command_rpm)
                .current_dir(&src_path)
                .spawn()
                .expect("failed to execute zip command");
            if !zip_output.wait().unwrap().success() {
                println!("zip failed");
                return Err(anyhow::anyhow!(task_id));
            }

            let mut installer = HashMap::new(); 
            installer.insert(installer_deb.as_str(), md5_deb.as_str());
            installer.insert(installer_rpm.as_str(), md5_rpm.as_str());
            backup(src_path, out_dir.as_str(), "", config.clone(), &server_addr, task_id, db_pool, &oem, installer, HashMap::new()).await?;
        }
        else {
            let (installer, md5) = calc_installer_md5(dst.to_str().unwrap(), "exe").await;
            let mut installer_map = HashMap::new();
            installer_map.insert(installer.as_str(), md5.as_str());
            backup(src_path, out_dir.as_str(), "", config.clone(), &server_addr, task_id, db_pool, &oem, installer_map, HashMap::new()).await?;
        }
    }

    println!("Task {} finished", task_id);

    Ok(())
}

pub async fn init_db() -> anyhow::Result<sqlx::SqlitePool> {
    let mut database_path = PathBuf::from(env::current_dir()?);

    let mut file = File::open("config.toml")
        .await
        .expect("failed to open config file");

    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();
    let config: toml::Value = toml::from_str(&contents).unwrap();

    let db_name = config["src"]["db"].as_str().unwrap_or_default();

    let need_close = db_name.is_empty();

    //init env
    env::set_var("XN_BUILD", "1");
    let sign_server = config.get("sign").map_or("", |v| v.as_str().unwrap_or(""));
    env::set_var("SNOW_SIGN_ADDRESS", sign_server);
    if let Some(dev_tools) = config.get("dev_tools") {
        if let Some(dev_path) = dev_tools.get(std::env::consts::OS) {
            let current_path = env::var("PATH").unwrap_or_default();
            let separator = if cfg!(windows) { ";" } else { ":" };
            let env_additon = format!("{}{}{}", dev_path.as_str().unwrap(),separator, current_path);
            env::set_var("PATH", env_additon.clone());
        }
    }

    if let Some(python) = config.get("python") {
        if let Some(python_path) = python.get(std::env::consts::OS) {
            let current_path = env::var("PATH").unwrap_or_default();
            let separator = if cfg!(windows) { ";" } else { ":" };
            let env_additon = format!("{}{}{}", python_path.as_str().unwrap(),separator, current_path);
            env::set_var("PATH", env_additon.clone());
        }
    }
    //init env

    if db_name.is_empty() {
        database_path.push("pkg.db");
    } else {
        database_path.push(db_name);
    }
    println!("database path: {:?}", database_path);

    if !database_path.exists() {
        let _ = std::fs::File::create(&database_path)?;
    }
    let database_url = format!("sqlite://{}", database_path.to_str().unwrap());

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    pool.execute(
        r#"
        CREATE TABLE IF NOT EXISTS pkg (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            start_time TEXT NOT NULL,
            end_time TEXT,
            branch_name TEXT NOT NULL,
            oem_name TEXT,
            commit_id TEXT,
            is_signed BOOLEAN,
            is_increment BOOLEAN,
            storage_path TEXT,
            installer TEXT,
            state TEXT,
            server TEXT
        );
        "#,
    )
    .await?;

    let records: Vec<sqlx::sqlite::SqliteRow> = sqlx::query(TASKLIST_QUERY)
        .fetch_all(&pool)
        .await
        .expect("failed to fetch records");

    for row in records.iter() {
        let state = row.get::<String, _>("state");
        if state != "success" && state != "failed" {
            let id = row.get::<i64, _>("id");
            sqlx::query("DELETE FROM pkg WHERE id = ?")
                .bind(id)
                .execute(&pool)
                .await
                .expect("failed to delete task");
        }
    }

    if need_close {
        pool.close().await;
    }

    Ok(pool)
}
