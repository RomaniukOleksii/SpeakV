use anyhow::Result;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpdateAvailable(String), // version string
    NoUpdateAvailable,
    Downloading,
    Installing,
    Error(String),
    Success,
}

pub struct UpdateManager {
    pub status: Arc<Mutex<UpdateStatus>>,
    pub current_version: String,
}

impl UpdateManager {
    pub fn new() -> Self {
        Self {
            status: Arc::new(Mutex::new(UpdateStatus::Idle)),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn check_for_updates(&self, repo_owner: &str, repo_name: &str) {
        let status = self.status.clone();
        let owner = repo_owner.to_string();
        let name = repo_name.to_string();
        let current = self.current_version.clone();

        tokio::spawn(async move {
            // Set status to checking
            if let Ok(mut s) = status.lock() {
                *s = UpdateStatus::Checking;
            }

            // Check for updates using GitHub API
            match Self::fetch_latest_version(&owner, &name).await {
                Ok(latest_version) => {
                    if Self::is_newer_version(&current, &latest_version) {
                        if let Ok(mut s) = status.lock() {
                            *s = UpdateStatus::UpdateAvailable(latest_version);
                        }
                    } else {
                        if let Ok(mut s) = status.lock() {
                            *s = UpdateStatus::NoUpdateAvailable;
                        }
                    }
                }
                Err(e) => {
                    if let Ok(mut s) = status.lock() {
                        *s = UpdateStatus::Error(format!("Failed to check updates: {}", e));
                    }
                }
            }
        });
    }

    async fn fetch_latest_version(owner: &str, repo: &str) -> Result<String> {
        let url = format!("https://api.github.com/repos/{}/{}/releases/latest", owner, repo);
        
        let client = reqwest::Client::builder()
            .user_agent("SpeakV-Updater")
            .build()?;
        
        let response = client.get(&url).send().await?;
        let json: serde_json::Value = response.json().await?;
        
        if let Some(tag) = json["tag_name"].as_str() {
            // Remove 'v' prefix if present
            let version = tag.trim_start_matches('v');
            Ok(version.to_string())
        } else {
            Err(anyhow::anyhow!("No tag_name found in release"))
        }
    }

    fn is_newer_version(current: &str, latest: &str) -> bool {
        // Simple version comparison (you can use semver crate for more robust comparison)
        let current_parts: Vec<u32> = current.split('.').filter_map(|s| s.parse().ok()).collect();
        let latest_parts: Vec<u32> = latest.split('.').filter_map(|s| s.parse().ok()).collect();
        
        for i in 0..3 {
            let c = current_parts.get(i).unwrap_or(&0);
            let l = latest_parts.get(i).unwrap_or(&0);
            
            if l > c {
                return true;
            } else if l < c {
                return false;
            }
        }
        
        false
    }

    pub fn download_and_install(&self, repo_owner: &str, repo_name: &str) {
        let status = self.status.clone();
        let owner = repo_owner.to_string();
        let name = repo_name.to_string();

        tokio::spawn(async move {
            // Set status to downloading
            if let Ok(mut s) = status.lock() {
                *s = UpdateStatus::Downloading;
            }

            match Self::perform_update(&owner, &name).await {
                Ok(_) => {
                    if let Ok(mut s) = status.lock() {
                        *s = UpdateStatus::Success;
                    }
                }
                Err(e) => {
                    if let Ok(mut s) = status.lock() {
                        *s = UpdateStatus::Error(format!("Update failed: {}", e));
                    }
                }
            }
        });
    }

    async fn perform_update(owner: &str, repo: &str) -> Result<()> {
        let owner = owner.to_string();
        let repo = repo.to_string();
        
        // Run blocking operations in a separate thread
        tokio::task::spawn_blocking(move || {
            let target = self_update::get_target();
            
            let releases = self_update::backends::github::ReleaseList::configure()
                .repo_owner(&owner)
                .repo_name(&repo)
                .build()?
                .fetch()?;

            if releases.is_empty() {
                return Err(anyhow::anyhow!("No releases found"));
            }

            let status = self_update::backends::github::Update::configure()
                .repo_owner(&owner)
                .repo_name(&repo)
                .bin_name("speakv")
                .target(&target)
                .current_version(env!("CARGO_PKG_VERSION"))
                .build()?
                .update()?;

            println!("Update status: `{}`!", status.version());
            Ok(())
        }).await?
    }
}
