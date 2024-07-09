use std::env;
use std::path::PathBuf;
use std::process::exit;
use std::process::Command;

use entity::db_enums::RepoSyncStatus;
use regex::Regex;
use sea_orm::ActiveModelTrait;
use sea_orm::Set;
use sea_orm::Unchanged;
use url::Url;
use walkdir::WalkDir;

use crate::kafka;
use crate::kafka::RepoMessage;
use crate::util;

pub async fn add_and_push_to_remote(workspace: PathBuf) {
    let conn = util::db_connection().await;
    let producer = kafka::get_producer();
    for entry in WalkDir::new(workspace)
        .max_depth(2)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_dir() && entry.depth() == 2 {
            if let Err(err) = env::set_current_dir(entry.path()) {
                eprintln!("Error changing directory: {}", err);
                exit(1);
            }

            let crate_name = entry.file_name().to_str().unwrap().to_owned();
            let mut record = crate::get_record(&conn, &crate_name).await;
            if record.status == Unchanged(RepoSyncStatus::Succeed) {
                continue;
            }

            let output = Command::new("git")
                .arg("remote")
                .arg("-v")
                .output()
                .unwrap();

            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // Create a regular expression pattern to match URLs
                let re = Regex::new(r"https://github\.com/[^\s]+").unwrap();

                let mut capture = re.captures_iter(&stdout);
                if let Some(capture) = capture.next() {
                    let mut url = Url::parse(&capture[0]).unwrap();
                    record.github_url = Set(Some(url.to_string()));
                    url.set_host(Some("localhost")).unwrap();
                    url.set_scheme("http").unwrap();
                    url.set_port(Some(8000)).unwrap();
                    let path = url.path().to_owned();
                    let new_path = format!("/third-part{}", path);
                    url.set_path(&new_path);

                    println!("Found URL: {}", url);
                    record.mega_url = Set(new_path);

                    Command::new("git")
                        .arg("remote")
                        .arg("remove")
                        .arg("nju")
                        .output()
                        .unwrap();

                    Command::new("git")
                        .arg("remote")
                        .arg("add")
                        .arg("nju")
                        .arg(url.to_string())
                        .output()
                        .unwrap();
                    let push_res = Command::new("git").arg("push").arg("nju").output().unwrap();
                    Command::new("git")
                        .arg("push")
                        .arg("nju")
                        .arg("--tags")
                        .output()
                        .unwrap();

                    if push_res.status.success() {
                        record.status = Set(RepoSyncStatus::Succeed);
                        record.err_message = Set(None);
                        let kafka_payload: RepoMessage = record.clone().into();
                        kafka::producer::send_message(
                            &producer,
                            &env::var("KAFKA_TOPIC").unwrap(),
                            bincode::serialize(&kafka_payload).unwrap(),
                        )
                        .await;
                    } else {
                        record.status = Set(RepoSyncStatus::Failed);
                        record.err_message =
                            Set(Some(String::from_utf8_lossy(&push_res.stderr).to_string()));
                    }
                    record.updated_at = Set(chrono::Utc::now().naive_utc());
                    record.save(&conn).await.unwrap();

                    println!("Push res: {}", String::from_utf8_lossy(&push_res.stdout));
                    println!("Push err: {}", String::from_utf8_lossy(&push_res.stderr));
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("Error running 'git remote -v':\n{}", stderr);
            }
        }
    }
}
