//! Google Cloud Application Default Credentials (ADC) for the Vertex AI
//! (Gemini) provider — the GCP analog of the AWS credential chain that
//! backs Bedrock. After `gcloud auth application-default login`, the ADC
//! file at `~/.config/gcloud/application_default_credentials.json` (or
//! `GOOGLE_APPLICATION_CREDENTIALS`, or the GCE metadata server) yields
//! OAuth2 access tokens; `gcp_auth` discovers, caches, and refreshes
//! them, so no API key is ever involved.

use std::path::PathBuf;
use std::sync::Arc;

use gcp_auth::{
    ConfigDefaultCredentials, CustomServiceAccount, GCloudAuthorizedUser,
    MetadataServiceAccount, TokenProvider,
};
use tokio::sync::OnceCell;

/// Scope Vertex AI generateContent calls need.
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

static PROVIDER: OnceCell<Arc<dyn TokenProvider>> = OnceCell::const_new();

/// The ADC token provider, discovered once per process. Same chain as
/// `gcp_auth::provider()` — `GOOGLE_APPLICATION_CREDENTIALS` service
/// account, the gcloud ADC file, the GCE metadata server, then the
/// `gcloud` CLI itself — except that a `GOOGLE_APPLICATION_CREDENTIALS`
/// pointing at a non-service-account file (commonly the
/// `authorized_user` ADC file itself) is skipped instead of aborting
/// the whole chain, which is what `gcp_auth::provider()` does.
/// Discovery failure is NOT cached, so running
/// `gcloud auth application-default login` after server start works on
/// the next request.
async fn provider() -> Result<&'static Arc<dyn TokenProvider>, String> {
    PROVIDER
        .get_or_try_init(|| async {
            discover_provider().await.map_err(|e| {
                format!("no GCP credentials ({e}); run `gcloud auth application-default login`")
            })
        })
        .await
}

async fn discover_provider() -> Result<Arc<dyn TokenProvider>, String> {
    if let Ok(path) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        if !path.is_empty() {
            // Not a service account (e.g. an authorized_user ADC file)?
            // Fall through to the other ADC sources instead of failing.
            if let Ok(p) = CustomServiceAccount::from_file(&path) {
                return Ok(Arc::new(p));
            }
        }
    }
    if let Ok(p) = ConfigDefaultCredentials::new().await {
        return Ok(Arc::new(p));
    }
    if let Ok(p) = MetadataServiceAccount::new().await {
        return Ok(Arc::new(p));
    }
    if let Ok(p) = GCloudAuthorizedUser::new().await {
        return Ok(Arc::new(p));
    }
    Err("no ADC source worked".into())
}

/// A fresh-enough OAuth2 access token. gcp_auth caches tokens and only
/// re-fetches once the current one expires, so per-request cost after
/// the first call is a lock check.
pub(crate) async fn access_token() -> Result<String, String> {
    let token = provider()
        .await?
        .token(&[CLOUD_PLATFORM_SCOPE])
        .await
        .map_err(|e| format!("failed to get GCP access token: {e}"))?;
    Ok(token.as_str().to_string())
}

/// The quota project of the ADC credentials themselves (the
/// `quota_project_id` in the ADC file, or the gcloud CLI's project).
/// Distinct from [`project_id`]: this ignores the `VERTEX_PROJECT_ID` /
/// `GOOGLE_CLOUD_PROJECT` overrides, which is what you want for the
/// `x-goog-user-project` quota header.
pub(crate) async fn adc_quota_project() -> Result<String, String> {
    provider()
        .await?
        .project_id()
        .await
        .map(|p| p.to_string())
        .map_err(|e| format!("no quota project in ADC: {e}"))
}

/// The GCP project to bill Vertex AI calls to. Resolution order:
/// `VERTEX_PROJECT_ID` / `GOOGLE_CLOUD_PROJECT` env, the auth context
/// (quota project in the ADC file, or the gcloud CLI's project), then
/// the gcloud CLI config file.
pub(crate) async fn project_id() -> Result<String, String> {
    for var in ["VERTEX_PROJECT_ID", "GOOGLE_CLOUD_PROJECT"] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                return Ok(p);
            }
        }
    }
    if let Ok(p) = provider().await?.project_id().await {
        if !p.is_empty() {
            return Ok(p.to_string());
        }
    }
    if let Some(p) = gcloud_config_project() {
        return Ok(p);
    }
    Err("no GCP project: set VERTEX_PROJECT_ID or `gcloud config set project <id>`".into())
}

/// Vertex AI region. `VERTEX_LOCATION` / `GOOGLE_CLOUD_LOCATION` env,
/// else `global` (the region-less endpoint, which serves Gemini).
pub(crate) fn location() -> String {
    ["VERTEX_LOCATION", "GOOGLE_CLOUD_LOCATION"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "global".into())
}

/// The Vertex AI endpoint base URL genai's Vertex adapter expects
/// (trailing slash; it appends `publishers/google/models/...`).
pub(crate) async fn vertex_endpoint() -> Result<String, String> {
    let project = project_id().await?;
    let location = location();
    Ok(match location.as_str() {
        "global" => {
            format!("https://aiplatform.googleapis.com/v1/projects/{project}/locations/global/")
        }
        loc => format!(
            "https://{loc}-aiplatform.googleapis.com/v1/projects/{project}/locations/{loc}/"
        ),
    })
}

/// Reads the active gcloud CLI configuration
/// (`$CLOUDSDK_CONFIG/configurations/config_default`, else
/// `~/.config/gcloud/configurations/config_default`) for `[core] project`.
fn gcloud_config_project() -> Option<String> {
    let dir = std::env::var_os("CLOUDSDK_CONFIG").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/gcloud"))
    })?;
    let content = std::fs::read_to_string(dir.join("configurations/config_default")).ok()?;
    let mut in_core = false;
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_core = line == "[core]";
        } else if in_core {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "project" && !v.trim().is_empty() {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}
