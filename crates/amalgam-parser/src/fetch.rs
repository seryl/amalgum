//! Fetch CRDs from URLs, GitHub repos, etc.

use crate::crd::CRD;
use anyhow::Result;
use futures::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

pub struct CRDFetcher {
    client: reqwest::Client,
    multi_progress: Arc<MultiProgress>,
}

impl CRDFetcher {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .user_agent("amalgam")
                .build()?,
            multi_progress: Arc::new(MultiProgress::new()),
        })
    }

    /// Fetch CRDs from a URL
    /// Supports:
    /// - Direct YAML files
    /// - GitHub repository URLs
    /// - GitHub directory listings
    pub async fn fetch_from_url(&self, url: &str) -> Result<Vec<CRD>> {
        let is_tty = atty::is(atty::Stream::Stdout);

        let main_spinner = if is_tty {
            let pb = self.multi_progress.add(ProgressBar::new_spinner());
            pb.set_style(
                ProgressStyle::default_spinner()
                    .template("{spinner:.cyan} {msg}")?
                    .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
            );
            pb.enable_steady_tick(Duration::from_millis(100));
            pb.set_message("Initializing CRD fetcher...");
            Some(pb)
        } else {
            None
        };

        let result = if url.contains("github.com") {
            self.fetch_from_github(url, is_tty).await
        } else if url.ends_with(".yaml") || url.ends_with(".yml") {
            // Direct YAML file
            if let Some(ref pb) = main_spinner {
                pb.set_message("Downloading YAML file...".to_string());
            } else {
                println!("Downloading YAML file from {}", url);
            }
            let content = self.client.get(url).send().await?.text().await?;
            let crd: CRD = serde_yaml::from_str(&content)?;
            Ok(vec![crd])
        } else {
            // Try to fetch as directory listing
            self.fetch_directory(url).await
        };

        if let Some(pb) = main_spinner {
            if let Ok(ref crds) = result {
                pb.finish_with_message(format!("✓ Successfully fetched {} CRDs", crds.len()));
            } else {
                pb.finish_with_message("✗ Failed to fetch CRDs");
            }
        } else if let Ok(ref crds) = result {
            println!("Successfully fetched {} CRDs", crds.len());
        }

        result
    }

    /// Fetch CRDs from a GitHub repository or directory
    async fn fetch_from_github(&self, url: &str, is_tty: bool) -> Result<Vec<CRD>> {
        // Convert GitHub URL to raw content URL
        let parts: Vec<&str> = url.split('/').collect();
        if parts.len() < 5 {
            return Err(anyhow::anyhow!("Invalid GitHub URL"));
        }

        let owner = parts[3];
        let repo = parts[4];

        // Find the path after tree/branch
        let (path, branch) = if let Some(tree_idx) = parts.iter().position(|&p| p == "tree") {
            if parts.len() > tree_idx + 2 {
                let branch = parts[tree_idx + 1];
                let path = parts[tree_idx + 2..].join("/");
                (path, branch)
            } else if parts.len() > tree_idx + 1 {
                let branch = parts[tree_idx + 1];
                (String::new(), branch)
            } else {
                (String::new(), "main")
            }
        } else if let Some(blob_idx) = parts.iter().position(|&p| p == "blob") {
            // Single file
            if parts.len() > blob_idx + 2 {
                let branch = parts[blob_idx + 1];
                let file_path = parts[blob_idx + 2..].join("/");
                let raw_url = format!(
                    "https://raw.githubusercontent.com/{}/{}/{}/{}",
                    owner, repo, branch, file_path
                );

                let pb = if is_tty {
                    let pb = self.multi_progress.add(ProgressBar::new_spinner());
                    pb.set_style(
                        ProgressStyle::default_spinner().template("{spinner:.cyan} {msg}")?,
                    );
                    pb.enable_steady_tick(Duration::from_millis(100));
                    pb.set_message(format!("Downloading {}", file_path));
                    Some(pb)
                } else {
                    println!("Downloading {}", file_path);
                    None
                };

                let content = self.client.get(&raw_url).send().await?.text().await?;
                let crd: CRD = serde_yaml::from_str(&content)?;

                if let Some(pb) = pb {
                    pb.finish_with_message(format!("✓ Downloaded {}", file_path));
                }

                return Ok(vec![crd]);
            }
            (String::new(), "main")
        } else {
            (String::new(), "main")
        };

        // Use GitHub API to list directory contents
        let api_url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
            owner, repo, path, branch
        );

        let listing_pb = if is_tty {
            let pb = self.multi_progress.add(ProgressBar::new_spinner());
            pb.set_style(ProgressStyle::default_spinner().template("{spinner:.cyan} {msg}")?);
            pb.enable_steady_tick(Duration::from_millis(100));
            pb.set_message(format!("Listing files from {}/{}/{}", owner, repo, path));
            Some(pb)
        } else {
            println!("Listing files from {}/{}/{}", owner, repo, path);
            None
        };

        let response = self
            .client
            .get(&api_url)
            .header("Accept", "application/vnd.github.v3+json")
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await?;
            return Err(anyhow::anyhow!("GitHub API error ({}): {}", status, text));
        }

        let files: Vec<GitHubContent> = response.json().await?;

        // Filter for YAML files that look like CRDs
        let yaml_files: Vec<_> = files
            .iter()
            .filter(|item| item.name.ends_with(".yaml") || item.name.ends_with(".yml"))
            .collect();

        if let Some(pb) = listing_pb {
            pb.finish_with_message(format!("✓ Found {} YAML files", yaml_files.len()));
        } else {
            println!("Found {} YAML files", yaml_files.len());
        }

        if yaml_files.is_empty() {
            return Ok(Vec::new());
        }

        // Create main progress bar for overall download progress
        let main_progress = if is_tty {
            let pb = self
                .multi_progress
                .add(ProgressBar::new(yaml_files.len() as u64));
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} {msg}")?
                    .progress_chars("##-")
            );
            pb.set_message("Overall progress");
            Some(Arc::new(pb))
        } else {
            None
        };

        // Download files concurrently with controlled parallelism
        let max_concurrent = 5;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let client = self.client.clone();
        let multi_progress = self.multi_progress.clone();
        let active_downloads = Arc::new(Mutex::new(Vec::new()));

        let total_files = yaml_files.len();
        let download_tasks = yaml_files.iter().enumerate().map(|(idx, item)| {
            let client = client.clone();
            let semaphore = semaphore.clone();
            let name = item.name.clone();
            let download_url = item.download_url.clone();
            let main_progress = main_progress.clone();
            let multi_progress = multi_progress.clone();
            let active_downloads = active_downloads.clone();

            async move {
                let _permit = semaphore.acquire().await.unwrap();

                // Create individual progress bar for this download
                let individual_pb = if is_tty {
                    let pb = multi_progress.add(ProgressBar::new_spinner());
                    pb.set_style(
                        ProgressStyle::default_spinner()
                            .template(&format!("  {{spinner:.yellow}} [{}] {{msg}}", idx + 1))
                            .unwrap()
                            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]),
                    );
                    pb.enable_steady_tick(Duration::from_millis(80));
                    pb.set_message(format!("Downloading {}", name));

                    // Track active download
                    active_downloads.lock().await.push(pb.clone());

                    Some(pb)
                } else {
                    println!("[{}/{}] Downloading {}", idx + 1, total_files, name);
                    None
                };

                let result = if let Some(url) = download_url {
                    match fetch_single_crd(&client, &url).await {
                        Ok(crd) => {
                            if let Some(ref pb) = individual_pb {
                                pb.finish_with_message(format!("✓ {}", name));
                            }
                            Some(crd)
                        }
                        Err(e) => {
                            if let Some(ref pb) = individual_pb {
                                pb.finish_with_message(format!("✗ {} ({})", name, e));
                            }
                            None
                        }
                    }
                } else {
                    if let Some(ref pb) = individual_pb {
                        pb.finish_with_message(format!("✗ {} (no download URL)", name));
                    }
                    None
                };

                // Update main progress
                if let Some(ref main_pb) = main_progress {
                    main_pb.inc(1);
                    let completed = main_pb.position();
                    let total = main_pb.length().unwrap_or(0);
                    main_pb.set_message(format!("Completed {}/{} files", completed, total));
                }

                // Remove from active downloads
                if let Some(ref pb) = individual_pb {
                    let mut active = active_downloads.lock().await;
                    active.retain(|p| !Arc::ptr_eq(&Arc::new(p.clone()), &Arc::new(pb.clone())));
                }

                result
            }
        });

        let mut stream = futures::stream::iter(download_tasks).buffer_unordered(max_concurrent);

        let mut crds = Vec::new();
        while let Some(result) = stream.next().await {
            if let Some(crd) = result {
                crds.push(crd);
            }
        }

        if let Some(ref main_pb) = main_progress {
            main_pb.finish_with_message(format!(
                "✓ Successfully downloaded {} valid CRDs",
                crds.len()
            ));
        } else {
            println!("Downloaded {} valid CRDs", crds.len());
        }

        Ok(crds)
    }

    async fn fetch_directory(&self, _url: &str) -> Result<Vec<CRD>> {
        // For now, just try to list files
        // In a real implementation, would need directory listing support
        Err(anyhow::anyhow!(
            "Directory listing not supported for non-GitHub URLs"
        ))
    }

    /// Clear all progress bars
    pub fn finish(&self) {
        self.multi_progress.clear().ok();
    }
}

async fn fetch_single_crd(client: &reqwest::Client, url: &str) -> Result<CRD> {
    let content = client.get(url).send().await?.text().await?;

    // Most CRDs are single YAML documents, try that first
    if let Ok(crd) = serde_yaml::from_str::<CRD>(&content) {
        return Ok(crd);
    }

    // If that fails, try parsing as a Value first to check kind
    let value: serde_yaml::Value = serde_yaml::from_str(&content)?;
    if value.get("kind")
        == Some(&serde_yaml::Value::String(
            "CustomResourceDefinition".to_string(),
        ))
    {
        let crd: CRD = serde_yaml::from_value(value)?;
        return Ok(crd);
    }

    Err(anyhow::anyhow!("Not a valid CRD"))
}

#[derive(Debug, serde::Deserialize)]
struct GitHubContent {
    name: String,
    #[allow(dead_code)]
    path: String,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    content_type: String,
    download_url: Option<String>,
}

impl Default for CRDFetcher {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn sample_crd() -> serde_json::Value {
        json!({
            "apiVersion": "apiextensions.k8s.io/v1",
            "kind": "CustomResourceDefinition",
            "metadata": {
                "name": "compositions.apiextensions.crossplane.io"
            },
            "spec": {
                "group": "apiextensions.crossplane.io",
                "names": {
                    "kind": "Composition",
                    "plural": "compositions",
                    "singular": "composition"
                },
                "versions": [{
                    "name": "v1",
                    "served": true,
                    "storage": true,
                    "schema": {
                        "openAPIV3Schema": {
                            "type": "object",
                            "properties": {
                                "spec": {
                                    "type": "object",
                                    "properties": {
                                        "compositeTypeRef": {
                                            "type": "object",
                                            "properties": {
                                                "apiVersion": {"type": "string"},
                                                "kind": {"type": "string"}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }]
            }
        })
    }

    #[tokio::test]
    async fn test_fetch_single_yaml_file() {
        let mock_server = MockServer::start().await;

        let crd_yaml = serde_yaml::to_string(&sample_crd()).unwrap();

        Mock::given(method("GET"))
            .and(path("/test.yaml"))
            .respond_with(ResponseTemplate::new(200).set_body_string(crd_yaml))
            .mount(&mock_server)
            .await;

        let fetcher = CRDFetcher::new().unwrap();
        let url = format!("{}/test.yaml", &mock_server.uri());
        let crds = fetcher.fetch_from_url(&url).await.unwrap();

        assert_eq!(crds.len(), 1);
        assert_eq!(crds[0].spec.group, "apiextensions.crossplane.io");
        assert_eq!(crds[0].spec.names.kind, "Composition");
    }

    #[tokio::test]
    async fn test_error_handling_404() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/missing.yaml"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let fetcher = CRDFetcher::new().unwrap();
        let url = format!("{}/missing.yaml", &mock_server.uri());
        let result = fetcher.fetch_from_url(&url).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_error_handling_invalid_yaml() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/invalid.yaml"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not: valid: yaml: content:"))
            .mount(&mock_server)
            .await;

        let fetcher = CRDFetcher::new().unwrap();
        let url = format!("{}/invalid.yaml", &mock_server.uri());
        let result = fetcher.fetch_from_url(&url).await;

        assert!(result.is_err());
    }
}
