//! User-triggered checks for the configured public Orchard distribution.
//!
//! The release repository is deliberately compiled in through
//! `ORCHARD_UPDATE_REPOSITORY`; a source checkout with no distribution
//! repository does not make network requests or guess a GitHub project.

use std::{fmt, time::Duration};

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, USER_AGENT};
use semver::Version as SemVersion;
use serde::Deserialize;

const RELEASE_API: &str = "https://api.github.com/repos";
const ASSET_SUFFIX: &str = "-macos-arm64.zip";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RELEASE_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateCheck {
    pub status: UpdateStatus,
    pub latest_version: String,
    pub release_url: String,
    pub download_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    DistributionNotConfigured,
    InvalidRepository,
    InvalidVersion(String),
    NoRelease,
    Network(String),
    InvalidRelease(String),
    MissingPlatformAsset { expected: String },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DistributionNotConfigured => {
                write!(f, "updates are not configured for this build")
            }
            Self::InvalidRepository => write!(f, "the configured update repository is invalid"),
            Self::InvalidVersion(version) => write!(f, "invalid Orchard version {version:?}"),
            Self::NoRelease => write!(f, "the configured distribution has no stable release"),
            Self::Network(message) => write!(f, "could not check for updates: {message}"),
            Self::InvalidRelease(message) => {
                write!(f, "the latest release response is invalid: {message}")
            }
            Self::MissingPlatformAsset { expected } => write!(
                f,
                "the latest release has no macOS Apple Silicon asset named {expected}"
            ),
        }
    }
}

impl std::error::Error for UpdateError {}

/// Checks GitHub's latest stable release once. This function never downloads,
/// installs, or schedules an update.
pub async fn check_for_updates(current_version: &str) -> Result<UpdateCheck, UpdateError> {
    let repository = option_env!("ORCHARD_UPDATE_REPOSITORY")
        .filter(|value| !value.trim().is_empty())
        .ok_or(UpdateError::DistributionNotConfigured)?;
    check_with_api(current_version, repository, RELEASE_API).await
}

async fn check_with_api(
    current_version: &str,
    repository: &str,
    api_base: &str,
) -> Result<UpdateCheck, UpdateError> {
    let current = parse_version(current_version)?;
    let repository = validate_repository(repository)?;
    let url = format!("{api_base}/{repository}/releases/latest");
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| UpdateError::Network(error.to_string()))?;
    let response = client
        .get(url)
        .header(USER_AGENT, "Orchard-update-check")
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| UpdateError::Network(error.to_string()))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(UpdateError::NoRelease);
    }
    let response = response
        .error_for_status()
        .map_err(|error| UpdateError::Network(error.to_string()))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RELEASE_RESPONSE_BYTES as u64)
    {
        return Err(UpdateError::InvalidRelease(
            "response exceeds 1 MiB".to_owned(),
        ));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| UpdateError::Network(error.to_string()))?;
        if body.len() + chunk.len() > MAX_RELEASE_RESPONSE_BYTES {
            return Err(UpdateError::InvalidRelease(
                "response exceeds 1 MiB".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let release = serde_json::from_slice::<GithubRelease>(&body)
        .map_err(|error| UpdateError::InvalidRelease(error.to_string()))?;
    release.into_check(current, repository)
}

fn validate_repository(value: &str) -> Result<&str, UpdateError> {
    let mut pieces = value.split('/');
    let owner = pieces.next().filter(|item| valid_repository_part(item));
    let name = pieces.next().filter(|item| valid_repository_part(item));
    if owner.is_some() && name.is_some() && pieces.next().is_none() {
        Ok(value)
    } else {
        Err(UpdateError::InvalidRepository)
    }
}

fn valid_repository_part(part: &&str) -> bool {
    *part != "."
        && *part != ".."
        && !part.is_empty()
        && part.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    prerelease: bool,
    draft: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

impl GithubRelease {
    fn into_check(self, current: SemVersion, repository: &str) -> Result<UpdateCheck, UpdateError> {
        if self.draft || self.prerelease || self.tag_name.contains('-') {
            return Err(UpdateError::InvalidRelease(
                "latest endpoint returned a draft or prerelease".to_owned(),
            ));
        }
        let latest = parse_version(&self.tag_name)?;
        let latest_version = latest.to_string();
        let expected_release_url = format!(
            "https://github.com/{repository}/releases/tag/{}",
            self.tag_name
        );
        if self.html_url != expected_release_url {
            return Err(UpdateError::InvalidRelease(
                "release page is outside the configured GitHub repository".to_owned(),
            ));
        }
        if latest <= current {
            return Ok(UpdateCheck {
                status: UpdateStatus::UpToDate,
                latest_version,
                release_url: self.html_url,
                download_url: None,
            });
        }
        let expected = format!("Orchard-{latest_version}{ASSET_SUFFIX}");
        let expected_download_url = format!(
            "https://github.com/{repository}/releases/download/{}/{expected}",
            self.tag_name
        );
        let download_url = self
            .assets
            .iter()
            .find(|asset| {
                asset.name == expected && asset.browser_download_url == expected_download_url
            })
            .map(|asset| asset.browser_download_url.clone())
            .ok_or_else(|| UpdateError::MissingPlatformAsset { expected })?;
        Ok(UpdateCheck {
            status: UpdateStatus::UpdateAvailable,
            latest_version,
            release_url: self.html_url,
            download_url: Some(download_url),
        })
    }
}

fn parse_version(input: &str) -> Result<SemVersion, UpdateError> {
    SemVersion::parse(input.strip_prefix('v').unwrap_or(input))
        .map_err(|_| UpdateError::InvalidVersion(input.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag_name: &str, assets: &[&str]) -> GithubRelease {
        GithubRelease {
            tag_name: tag_name.to_owned(),
            html_url: format!("https://github.com/acme/orchard/releases/tag/{tag_name}"),
            prerelease: false,
            draft: false,
            assets: assets
                .iter()
                .map(|name| GithubAsset {
                    name: (*name).to_owned(),
                    browser_download_url: format!(
                        "https://github.com/acme/orchard/releases/download/{tag_name}/{name}"
                    ),
                })
                .collect(),
        }
    }

    #[test]
    fn parses_semver_and_orders_prereleases() {
        assert!(parse_version("1.2.3-alpha.1").unwrap() < parse_version("1.2.3-alpha.2").unwrap());
        assert!(parse_version("1.2.3-alpha").unwrap() < parse_version("1.2.3").unwrap());
        assert!(parse_version("v2.0.0").unwrap() > parse_version("1.99.99").unwrap());
        assert!(parse_version("1.2").is_err());
        assert!(parse_version("01.2.3").is_err());
    }

    #[test]
    fn equal_and_older_releases_are_up_to_date() {
        for latest in ["1.2.3", "1.2.2"] {
            let check = release(latest, &[])
                .into_check(parse_version("1.2.3").unwrap(), "acme/orchard")
                .unwrap();
            assert_eq!(check.status, UpdateStatus::UpToDate);
            assert_eq!(check.download_url, None);
        }
    }

    #[test]
    fn newer_release_requires_the_apple_silicon_asset() {
        let error = release("v1.2.4", &[])
            .into_check(parse_version("1.2.3").unwrap(), "acme/orchard")
            .unwrap_err();
        assert_eq!(
            error,
            UpdateError::MissingPlatformAsset {
                expected: "Orchard-1.2.4-macos-arm64.zip".to_owned()
            }
        );
        let check = release("v1.2.4", &["Orchard-1.2.4-macos-arm64.zip"])
            .into_check(parse_version("1.2.3").unwrap(), "acme/orchard")
            .unwrap();
        assert_eq!(check.status, UpdateStatus::UpdateAvailable);
        assert_eq!(check.download_url.as_deref(), Some("https://github.com/acme/orchard/releases/download/v1.2.4/Orchard-1.2.4-macos-arm64.zip"));
    }

    #[test]
    fn prerelease_is_never_presented_as_stable() {
        let error = release("1.2.4-beta.1", &["Orchard-1.2.4-beta.1-macos-arm64.zip"])
            .into_check(parse_version("1.2.3").unwrap(), "acme/orchard")
            .unwrap_err();
        assert!(matches!(error, UpdateError::InvalidRelease(_)));
    }

    #[test]
    fn offline_errors_are_reported() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(check_with_api(
                "1.2.3",
                "acme/orchard",
                "http://127.0.0.1:9",
            ))
            .unwrap_err();
        assert!(matches!(error, UpdateError::Network(_)));
    }
}
