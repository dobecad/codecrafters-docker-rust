use anyhow::{anyhow, Context, Result};
use flate2::read::GzDecoder;
use reqwest;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::copy;
use std::io::{Cursor, Seek, SeekFrom};
use std::os::unix::fs;
use std::process::Stdio;
use tar::Archive;
use tempfile;
use tokio;

const CHROOT_DIR: &'static str = "/tmp/codecrafters";

// Usage: your_docker.sh run <image> <command> <arg1> <arg2> ...
#[tokio::main]
async fn main() -> Result<()> {
    // Parse args, auth, and pull image before filesystem and PID isolation
    let client = reqwest::Client::new();
    let args: Vec<_> = std::env::args().collect();
    let image = &args[2];
    let command = &args[3];
    let command_args = &args[4..];

    // Download and unpack target image into the newly created chroot directory
    let (image_name, image_tag) = parse_image(image)?;
    let token = auth(&client, image).await?;
    let manifest = fetch_manifest(&client, &image_name, &image_tag, &token).await?;
    let image_manifest = fetch_image_manifest(&client, &image_name, &token, &manifest).await?;
    let _ = download_image_from_manifest(&client, &image_name, &token, &image_manifest).await?;

    // Create the chroot directory and the necessary child directories
    let _ = std::fs::create_dir_all(CHROOT_DIR).context("failed to create chroot directory")?;
    let _ = std::fs::create_dir_all(format!("{}/usr/local/bin", CHROOT_DIR))
        .context("failed to create chroot /usr/local/bin directory")?;
    let _ = std::fs::create_dir_all(format!("{}/dev/null", CHROOT_DIR))
        .context("failed to create chroot /dev/null directory")?;

    // Copy the docker-explorer binary to the chroot /usr/local/bin directory
    let _ = copy(
        "/usr/local/bin/docker-explorer",
        format!("{}/usr/local/bin/docker-explorer", CHROOT_DIR),
    )
    .context("failed to copy docker-explorer")?;

    // Change root directory of current process to our chroot directory we just created
    let _ = fs::chroot(CHROOT_DIR).context("failed to chroot")?;

    // Isolate process
    unsafe {
        libc::unshare(libc::CLONE_NEWPID);
    };

    let output = std::process::Command::new(command)
        .args(command_args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| {
            format!(
                "Tried to run '{}' with arguments {:?}",
                command, command_args
            )
        })?;

    // Use child process exit code, fallback to 1
    let code = output.status.code().unwrap_or(1);

    std::process::exit(code);
}

/// Parse out the image name and tag.
///
/// I am assuming we should always get something name:tag
fn parse_image(image: &str) -> Result<(String, String)> {
    let parsed_image_str: Vec<&str> = image.split(':').collect();
    if parsed_image_str.len() == 1 {
        return Ok((parsed_image_str[0].to_string(), "latest".to_string()));
    }
    if parsed_image_str.len() == 2 {
        let (name, tag) = (parsed_image_str[0], parsed_image_str[1]);
        return Ok((name.to_string(), tag.to_string()));
    }

    Err(anyhow!("Unexpected image name"))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TokenResponse {
    pub token: String,
    pub expires_in: i32,
    pub issued_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ManifestResponse {
    pub manifests: Vec<Manifest>,
    pub media_type: String,
    pub schema_version: u8,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Manifest {
    pub digest: String,
    pub media_type: String,
    pub platform: ManifestPlatform,
    pub size: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ManifestPlatform {
    pub architecture: String,
    pub os: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ImageManifest {
    pub schema_version: u8,
    pub media_type: String,
    pub config: ImageConfig,
    pub layers: Vec<ImageLayer>,
    pub subject: Option<ImageSubject>,
    pub annotations: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ImageConfig {
    pub media_type: String,
    pub digest: String,
    pub size: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ImageLayer {
    pub media_type: String,
    pub digest: String,
    pub size: u32,
    pub urls: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ImageSubject {
    pub media_type: String,
    pub digest: String,
    pub size: u32,
}

/// Fetch an auth token for our image, with only the pull scope
async fn auth(client: &reqwest::Client, image: &str) -> Result<String> {
    let (image_name, _) = parse_image(image).unwrap();
    let request = format!(
        "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/{image_name}:pull",
    );
    let response: TokenResponse = client
        .get(request)
        .send()
        .await
        .context("failed to send request")?
        .json()
        .await
        .context("failed to deserialize json response")?;
    Ok(response.token)
}

/// We need to initially fetch the manifests associated with the target image. This will return
/// a [`ManifestResponse`], which contains info about which digests are associated with
/// which platform specific images (i.e. linux/amd64, linux/arm, ...)
async fn fetch_manifest(
    client: &reqwest::Client,
    image_name: &str,
    image_tag: &str,
    token: &str,
) -> Result<ManifestResponse> {
    let request =
        format!("https://registry.hub.docker.com/v2/library/{image_name}/manifests/{image_tag}",);

    let response = client
        .get(&request)
        .bearer_auth(token)
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .context("failed to fetch manifest")?
        .text()
        .await;

    println!("Response: {:?}", response);

    let response: ManifestResponse = client
        .get(request)
        .bearer_auth(token)
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await
        .context("failed to fetch manifest")?
        .json()
        .await
        .context("failed to deserialize manifest")?;

    Ok(response)
}

/// After fetching all the image manifests for a particular image, we need to hit the same endpoint
/// again using the digest for the platform specific image we want layers for. For example,
/// if we originall hit ubuntu/manifests/latest, and we are in a linux/amd64 image, we will
/// want to use the digest where `manifest.platform.architecture == "amd64"` and
/// `manifest.platform.os == "linux"`
///
/// Then, we hit the same endpoint with that digest, which will look something like:
/// `https://registry.hub.docker.com/v2/library/ubuntu/manifests/sha256:c9cf959fd83770dfdefd8fb42cfef0761432af36a764c077aed54bbc5bb25368`
///
/// This will produce our [`ImageManifest`], which contains the information about our layers that we want
async fn fetch_image_manifest(
    client: &reqwest::Client,
    image_name: &str,
    token: &str,
    manifest: &ManifestResponse,
) -> Result<ImageManifest> {
    let target_images: Vec<(String, String)> = manifest
        .manifests
        .iter()
        .filter(|m| m.platform.architecture == "amd64" && m.platform.os == "linux")
        .map(|m| {
            let digest = m.digest.clone();
            let media_type = m.media_type.clone();
            (digest, media_type)
        })
        .collect();

    for (digest, media_type) in target_images.iter() {
        let request =
            format!("https://registry.hub.docker.com/v2/library/{image_name}/manifests/{digest}",);

        let response: ImageManifest = client
            .get(request)
            .bearer_auth(token)
            .header("Accept", media_type)
            .send()
            .await
            .context("failed to fetch manifest")?
            .json()
            .await
            .context("failed to deserialize manifest")?;

        // We only care about first digest
        return Ok(response);
    }

    return Err(anyhow!("Failed to get platform image digest"));
}

/// Now that we have our image manifest for our platform, we can download and unpack the image
/// to our chroot'ed directory
async fn download_image_from_manifest(
    client: &reqwest::Client,
    image_name: &str,
    token: &str,
    manifest: &ImageManifest,
) -> Result<()> {
    for layer in manifest.layers.iter() {
        let request = format!(
            "https://registry.hub.docker.com/v2/library/{image_name}/blobs/{}",
            &layer.digest
        );
        let image_layer_response = client
            .get(request)
            .bearer_auth(token)
            .header(reqwest::header::ACCEPT, &layer.media_type)
            .send()
            .await
            .context("failed to download image layer")?
            .bytes()
            .await
            .context("failed to get back bytes for layer")?;

        // Use a cursor to write bytes to a temporary file, which we will then unpack to our chroot'ed directory
        let mut bytes = Cursor::new(image_layer_response);
        let mut file = tempfile::tempfile().context("failed to create tempfile")?;
        std::io::copy(&mut bytes, &mut file).context("failed to copy layer bytes to temp file")?;

        file.seek(SeekFrom::Start(0))
            .context("failed to start seeking at beginning of file")?;
        let decoded = GzDecoder::new(file);
        Archive::new(decoded)
            .unpack(CHROOT_DIR)
            .context("failed to unpack archive")?;
    }

    Ok(())
}
