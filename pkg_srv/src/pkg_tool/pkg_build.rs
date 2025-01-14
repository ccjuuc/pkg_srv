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

const OUT_DIR: &str = "out/Release";
const OEM_KEY: &str = "current_xn_brand";

#[cfg(target_os = "macos")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-C"];
    pub const IDE: &str = "xcode";
    pub const INSTALLER_PROJECT: &str = "chrome/installer/mac";
}

#[cfg(target_os = "linux")]
mod os {
    pub const SHELL: [&str; 2] = ["sh", "-C"];
    pub const IDE: &str = "";
    pub const INSTALLER_PROJECT: &str = "chrome/installer/linux:stable";
}

#[cfg(windows)]
mod os {
    pub const SHELL: [&str; 2] = ["cmd.exe", "/c"];
    pub const IDE: &str = "vs2022";
    pub const INSTALLER_PROJECT: &str = "mini_installer";
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
    md5: String,
    storage_path: String,
    installer: String,
    state: String,
    server: String,
}

const TASKLIST_QUERY: &str = r#"
  SELECT id, start_time, branch_name, end_time, oem_name, commit_id, is_signed, is_increment, md5, storage_path,installer, state, server
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
SET end_time = ?, md5 = ?, storage_path = ?, installer = ?, state = ?
WHERE id = ?
"#;

#[derive(Deserialize, Clone)]
pub struct PkgBuildRequest {
    branch: String,
    commit_id: Option<String>,
    #[serde(default = "default_platform")]
    platform: String,
    #[serde(default = "default_is_64bit")]
    is_64bit: bool,
    is_increment: bool,
    is_signed: bool,
    server: String,
    oem_name: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateTaskRequest {
    id: i64,
    end_time: String,
    md5: String,
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

fn default_is_64bit() -> bool {
    true
}

fn default_platform() -> String {
    "windows".to_string()
}

fn print_info(msg: &[u8]) {
    let stdout_str = String::from_utf8_lossy(msg);
    let stdout_trimmed = match stdout_str.find('[') {
        Some(index) => &stdout_str[..index],
        None => &stdout_str,
    };
    println!("{}", stdout_trimmed);
}

pub async fn platform_cfg() -> impl IntoResponse {
    let mut file = File::open("config.toml").await.unwrap();
    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();

    let config: toml::Value = toml::from_str(&contents).unwrap();
    let server_config = serde_json::to_string(&config["server"]).unwrap();

    (StatusCode::OK, server_config)
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
        Ok(_) => {
        }
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete task".into()),
    }

    

    (StatusCode::OK, "Task deleted")
}

pub async fn tasklist(State(db_pool): State<SqlitePool>) -> impl IntoResponse {
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
            md5: row.get("md5"),
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
    let server_addr = config["git"]["addr"].as_str().unwrap();
    let server_user = config["git"]["user"].as_str().unwrap();
    let server_addr = format!("ssh://{}@{}", server_user, server_addr);
    println!("git address: {}", server_addr);

    let src_path = config["src"]["path"].as_str().unwrap();
    println!("Source code path: {}", src_path);

    println!("Branch: {}", payload.branch);
    println!("Commit ID: {:?}", payload.commit_id);
    println!("Platform: {}", payload.platform);
    println!("Is 64-bit: {}", payload.is_64bit);

    let payload_clone = payload.clone();
    let db_pool_clone = db_pool.clone();
    task::spawn(async move {
        if let Err(e) = do_build(&payload_clone, &db_pool_clone).await {
            let task_id = e.downcast_ref::<i64>().unwrap_or(&-1);
            update_task_state(
                &server_addr,
                *task_id,
                "",
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
        &payload.md5,
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
    md5: &str,
    store_path: &str,
    installer: &str,
    state: &str,
    db_pool: &SqlitePool,
) {
    if !db_pool.is_closed() {
        sqlx::query(UPDATE_TASK)
            .bind(&end_time)
            .bind(&md5)
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
                "md5": md5,
                "store_path": store_path,
                "installer": installer,
                "state": state,
            }))
            .send()
            .await
            .expect("failed to update_task");
    }
}

async fn backup(oem_name:&str, out_dir: &str, backup_dir: &str, installer:&str) {
    let _ = fs::copy(&Path::new(out_dir).join(installer), Path::new(backup_dir).join(installer)).await;
    for entry in WalkDir::new(out_dir).max_depth(1).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let file_name_lower = file_name.to_lowercase();
            let oem_name_fix = if oem_name.is_empty() || oem_name == "normal" { "snow" } else { oem_name };
            let oem_name_lower = oem_name_fix.to_lowercase();
            if file_name.ends_with(".pdb") || file_name.ends_with(".dbg") || file_name.ends_with(".debug") {
                if !oem_name.is_empty() && file_name_lower.contains(&oem_name_lower) {
                    let _ = fs::copy(entry.path(), Path::new(backup_dir).join(file_name)).await;
                    continue;
                }
            }
        }
    }
}
async fn calc_installer_md5(pkg_path: &str) -> (String, String) {
    let mut installer_file = String::new();
    let mut md5 = String::new();
    for entry in WalkDir::new(pkg_path).max_depth(1).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                let version_regex = Regex::new(r"\d+\.\d+\.\d+\.\d+").unwrap();
                if version_regex.is_match(file_name) {
                    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
                        if extension != "pdb" && extension != "dbg" && extension != "debug" {
                            installer_file = path.to_string_lossy().to_string();
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
        md5,
    )
}
async fn do_build(payload: &PkgBuildRequest, db_pool: &SqlitePool) -> anyhow::Result<()> {
    let server_addr = payload.server.clone();
    let task_id = add_task_state(
        &server_addr,
        &payload.branch,
        payload.oem_name.as_deref().unwrap_or(""),
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
        md5: "".to_string(),
        storage_path: "".to_string(),
        installer: "".to_string(),
        state: "".to_string(),
        commit_id: payload.commit_id.clone().unwrap_or_default(),
        is_increment: false,
        oem_name: payload.oem_name.clone().unwrap_or_default(),
        server: payload.server.clone(),
    };

    update_task_state(&server_addr, task_id, "", "", "", "", "clean...", db_pool).await;

    let mut file = File::open("config.toml")
        .await
        .expect("failed to open config file");

    let mut contents = String::new();
    file.read_to_string(&mut contents).await.unwrap();
    let config: toml::Value = toml::from_str(&contents).unwrap();
    use std::path::Path;

    let src_path = config["src"]["path"].as_str().unwrap();
    println!("Source code path: {}", src_path);
    let output_path = Path::new(src_path).join(OUT_DIR);

    if !payload.is_increment {
        let _ = std::fs::remove_dir_all(&output_path);
    }

    update_task_state(
        &server_addr,
        task_id,
        "",
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

    let target_cpu = format!(
        "target_cpu=\\\"{}\\\"",
        if payload.is_64bit { "x64" } else { "x86" }
    );

    let mut args = vec![
        "is_debug=false",
        "is_component_build=false",
        "symbol_level=0",
        "blink_symbol_level=0",
        "v8_symbol_level=0",
        "enable_nacl=false",
        "is_clang=true",
        &target_cpu,
    ];


    if let Some(custom_args) = config.get("custom_args") {
        if let Some(oem_name) = &payload.oem_name {
            if !oem_name.is_empty() {
                if let Some(oem_key) = custom_args.get("oem_key") {
                    let oem_key_str = oem_key.as_str().unwrap();
                    let oem_args = format!("{}=\\\"{}\\\"", oem_key_str, oem_name);
                    args.push(Box::leak(oem_args.into_boxed_str()));
                } else {
                    let oem_args = format!("{}=\\\"{}\\\"", OEM_KEY, oem_name);
                    args.push(Box::leak(oem_args.into_boxed_str()));
                }
            }
        }
        if let Some(spec_args) = custom_args.get(std::env::consts::OS) {
            for arg in spec_args.as_array().unwrap() {
                if !arg.as_str().unwrap().is_empty() {
                    args.push(arg.as_str().unwrap());
                }
            }
        }
    }

    update_task_state(
        &server_addr,
        task_id,
        "",
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
        OUT_DIR,
        &format!("--args=\"{}\"", args.join(" ")),
        ide_args,
    ];
    
    if let Some(dev_tools) = config.get("dev_tools") {
        if let Some(dev_path) = dev_tools.get(std::env::consts::OS) {
            let current_path = env::var("PATH").unwrap_or_default();
            let env_additon = format!("{};{}", current_path, dev_path.as_str().unwrap());
            env::set_var("PATH", env_additon.clone());
        }
    }

    let gn_output: Output;
    if cfg!(target_os = "windows") {
        gn_output = Command::new(os::SHELL[0])
            .arg(os::SHELL[1])
            .raw_arg(gn_args.join(" "))
            .current_dir(&src_path)
            .output()
            .expect("failed to execute gn command");
    } else {
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

    update_task_state(
        &server_addr,
        task_id,
        "",
        "",
        "",
        "",
        "build pre_build",
        db_pool,
    )
    .await;
    let pre_build_command = format!("ninja -C {} pre_build", OUT_DIR);
    let mut pre_build_output = Command::new(os::SHELL[0])
        .arg(os::SHELL[1])
        .arg(&pre_build_command)
        .current_dir(&src_path)
        .spawn()
        .expect("failed to execute pre_build command");

    if !pre_build_output.wait().unwrap().success() {
        return Err(anyhow::anyhow!(task_id));
    }

    update_task_state(&server_addr, task_id, "", "", "", "", "build base", db_pool).await;
    let base_build_command = format!("ninja -C {} base", OUT_DIR);
    let mut base_build_output = Command::new(os::SHELL[0])
        .arg(os::SHELL[1])
        .arg(&base_build_command)
        .current_dir(&src_path)
        .spawn()
        .expect("failed to execute base build command");
    if !base_build_output.wait().unwrap().success() {
        return Err(anyhow::anyhow!(task_id));
    }

    update_task_state(
        &server_addr,
        task_id,
        "",
        "",
        "",
        "",
        "build chrome",
        db_pool,
    )
    .await;
    let chrome_build_command = format!("ninja -C {} chrome", OUT_DIR);
    let mut chrome_build_output = Command::new(os::SHELL[0])
        .arg(os::SHELL[1])
        .arg(&chrome_build_command)
        .current_dir(&src_path)
        .spawn()
        .expect("failed to execute chrome build command");
    
    if !chrome_build_output.wait().unwrap().success() {
        return Err(anyhow::anyhow!(task_id));
    }

    update_task_state(
        &server_addr,
        task_id,
        "",
        "",
        "",
        "",
        "build installer",
        db_pool,
    )
    .await;
    let mini_installer_command = format!("ninja -C {} {}", OUT_DIR, os::INSTALLER_PROJECT);
    let mut mini_installer_output = Command::new(os::SHELL[0])
        .arg(os::SHELL[1])
        .arg(&mini_installer_command)
        .current_dir(&src_path)
        .spawn()
        .expect("failed to execute build installer command");
    if !mini_installer_output.wait().unwrap().success() {
        return Err(anyhow::anyhow!(task_id));
    }

    let (installer, md5) = calc_installer_md5(output_path.to_str().unwrap()).await;

    update_task_state(
        &server_addr,
        task_id,
        "",
        &md5,
        "",
        &installer,
        "backup",
        db_pool,
    )
    .await;

    if let Some(backup_path) = config.get("backup_path") {
        if let Some(path) = backup_path.get(std::env::consts::OS) {
            let backup_dir = path.as_str().unwrap_or_default();
            if !backup_dir.is_empty() {
                let date_subfolder = chrono::Local::now().format("%Y-%m-%d-%H-%M").to_string();
                let date_dir = Path::new(backup_dir).join(&date_subfolder);
                fs::create_dir_all(&date_dir).await.unwrap();
                let end_time = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                update_task_state(
                    &server_addr,
                    task_id,
                    &end_time,
                    &md5,
                    date_dir.to_str().unwrap(),
                    &installer,
                    "success",
                    db_pool,
                )
                .await;
                backup(payload.oem_name.as_deref().unwrap_or(""), output_path.to_str().unwrap(), date_dir.to_str().unwrap(), &installer).await;
            }

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
            md5 TEXT,
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
