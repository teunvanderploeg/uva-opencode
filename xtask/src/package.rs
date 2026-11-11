use anyhow::{Context, Result, bail};
use base64::Engine;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256, Sha512};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Command,
    time::Duration,
};

const BROWSER_VERSION: &str = "158.0.8127.18";
const OPENCODE_VERSION: &str = "2.2.5";
const TARGETS: &[(&str, &str, &str)] = &[
    ("macos-arm64", "mac-arm64", "darwin-arm64"),
    ("macos-x64", "mac-x64", "darwin-x64"),
    ("windows-x64", "win64", "windows-x64-baseline"),
    ("linux-x64", "linux64", "linux-x64-baseline"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Artifact {
    url: String,
    sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct Vendors {
    browser: Artifact,
    opencode: Artifact,
    opencode_package: String,
    opencode_integrity: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct Lock {
    browser_version: String,
    opencode_version: String,
    license: Artifact,
    targets: BTreeMap<String, Vendors>,
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .https_only(true)
        .build()?)
}

fn cache() -> PathBuf {
    crate::root().join(".runtime/artifacts")
}
fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let len = file.read(&mut buffer)?;
        if len == 0 {
            break;
        }
        hash.update(&buffer[..len]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

async fn download(name: &str, url: &str, expected: Option<&str>) -> Result<(PathBuf, Artifact)> {
    fs::create_dir_all(cache())?;
    // Version changes produce a different cache key even when the human-readable name stays the same.
    let url_hash = format!("{:x}", Sha256::digest(url.as_bytes()));
    let destination = cache().join(format!("{}-{name}", &url_hash[..16]));
    if destination.is_file() {
        let sha256 = digest(&destination)?;
        if expected.is_none_or(|expected| expected == sha256) {
            return Ok((
                destination,
                Artifact {
                    url: url.into(),
                    sha256,
                },
            ));
        }
        bail!(
            "Cached artifact {name} failed its pinned SHA-256 check. Remove that cache file and retry."
        );
    }
    println!("Downloading {name}");
    let mut response = client()?.get(url).send().await?.error_for_status()?;
    let partial = destination.with_extension("part");
    let mut file = File::create(&partial)?;
    let mut total = 0usize;
    while let Some(chunk) = response.chunk().await? {
        total += chunk.len();
        anyhow::ensure!(
            total <= 1_000_000_000,
            "Vendor artifact exceeded its size limit."
        );
        file.write_all(&chunk)?;
    }
    file.sync_all()?;
    drop(file);
    let sha256 = digest(&partial)?;
    if let Some(expected) = expected {
        anyhow::ensure!(
            sha256 == expected,
            "Downloaded artifact {name} failed its pinned SHA-256 check."
        );
    }
    fs::rename(partial, &destination)?;
    Ok((
        destination,
        Artifact {
            url: url.into(),
            sha256,
        },
    ))
}

pub async fn lock_artifacts() -> Result<()> {
    let mut targets = BTreeMap::new();
    for (target, browser, opencode) in TARGETS {
        let browser_url = format!(
            "https://storage.googleapis.com/chrome-for-testing-public/{BROWSER_VERSION}/{browser}/chrome-{browser}.zip"
        );
        let (_, browser) = download(&format!("browser-{target}.zip"), &browser_url, None).await?;
        let mut metadata = None;
        for scope in ["@opencode", "@opencode-ai"] {
            let url =
                format!("https://registry.npmjs.org/{scope}%2fcli-{opencode}/{OPENCODE_VERSION}");
            let response = client()?.get(url).send().await?;
            if response.status() == 404 {
                continue;
            }
            metadata = Some(response.error_for_status()?.json::<Value>().await?);
            break;
        }
        let metadata = metadata.context("Pinned OpenCode package was not found.")?;
        let url = metadata["dist"]["tarball"]
            .as_str()
            .context("Missing npm tarball.")?;
        let integrity = metadata["dist"]["integrity"]
            .as_str()
            .context("Missing npm integrity.")?;
        let (path, opencode) = download(&format!("opencode-{target}.tgz"), url, None).await?;
        let expected = integrity
            .strip_prefix("sha512-")
            .context("Expected npm SHA-512 integrity.")?;
        let actual =
            base64::engine::general_purpose::STANDARD.encode(Sha512::digest(fs::read(path)?));
        anyhow::ensure!(
            actual == expected,
            "OpenCode npm SHA-512 integrity verification failed."
        );
        targets.insert(
            target.to_string(),
            Vendors {
                browser,
                opencode,
                opencode_package: metadata["name"]
                    .as_str()
                    .context("Missing npm package name.")?
                    .into(),
                opencode_integrity: integrity.into(),
            },
        );
    }
    let (_, license) = download(
        "OpenCode-LICENSE.txt",
        &format!(
            "https://raw.githubusercontent.com/anomalyco/opencode/v{OPENCODE_VERSION}/LICENSE"
        ),
        None,
    )
    .await?;
    let lock = Lock {
        browser_version: BROWSER_VERSION.into(),
        opencode_version: OPENCODE_VERSION.into(),
        license,
        targets,
    };
    fs::write(
        crate::root().join("vendor-lock.json"),
        format!("{}\n", serde_json::to_string_pretty(&lock)?),
    )?;
    println!("Wrote vendor-lock.json. Review and commit it with the release source.");
    Ok(())
}

fn host_target() -> Result<String> {
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    let target = format!("{os}-{arch}");
    anyhow::ensure!(
        TARGETS.iter().any(|(id, _, _)| *id == target),
        "Unsupported native package target: {target}"
    );
    Ok(target)
}

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .context("Could not execute build command.")?;
    anyhow::ensure!(status.success(), "Build command failed with {status}.");
    Ok(())
}

fn executable(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn unzip(path: &Path, destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(path)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .context("Browser archive contains an unsafe path.")?;
        anyhow::ensure!(
            safe_relative(&relative),
            "Browser archive contains an unsafe path."
        );
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        fs::create_dir_all(output.parent().unwrap())?;
        let mode = entry.unix_mode().unwrap_or(0o644);
        if mode & 0o170000 == 0o120000 {
            let mut link = String::new();
            entry.read_to_string(&mut link)?;
            anyhow::ensure!(
                safe_relative(Path::new(&link)),
                "Unsafe browser archive symlink."
            );
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, &output)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(link, &output)?;
        } else {
            std::io::copy(&mut entry, &mut File::create(&output)?)?;
            executable(&output, mode)?;
        }
    }
    Ok(())
}

fn extract_opencode(path: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    let mut found = false;
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(path)?));
    let binary = if cfg!(windows) {
        "opencode.exe"
    } else {
        "opencode"
    };
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry
            .path()?
            .file_name()
            .context("Invalid tar member.")?
            .to_string_lossy()
            .into_owned();
        let lower = name.to_ascii_lowercase();
        if name == binary
            || lower.starts_with("license")
            || lower.starts_with("notice")
            || lower.starts_with("copying")
        {
            let output = destination.join(&name);
            std::io::copy(&mut entry, &mut File::create(&output)?)?;
            if name == binary {
                executable(&output, 0o755)?;
                found = true;
            }
        }
    }
    anyhow::ensure!(found, "OpenCode archive contained no executable.");
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let output = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let target = fs::read_link(path)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, output)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(target, output)?;
        } else if kind.is_dir() {
            copy_tree(&path, &output)?;
        } else {
            fs::copy(path, output)?;
        }
    }
    Ok(())
}

fn notices(bundle: &Path) -> Result<()> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(crate::root())
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Could not collect Cargo dependency licenses."
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let notices = bundle.join("licenses");
    fs::create_dir_all(&notices)?;
    let mut report = Vec::new();
    for package in metadata["packages"]
        .as_array()
        .context("Missing Cargo package metadata.")?
    {
        if package["source"].is_null() {
            continue;
        }
        let name = package["name"].as_str().unwrap();
        let version = package["version"].as_str().unwrap();
        report.push(json!({"name":name,"version":version,"license":package["license"],"source":package["source"]}));
        let source = Path::new(package["manifest_path"].as_str().unwrap())
            .parent()
            .unwrap();
        let destination = notices.join(format!("{name}-{version}"));
        fs::create_dir_all(&destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let lower = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if lower.starts_with("license")
                || lower.starts_with("notice")
                || lower.starts_with("copying")
                || lower == "copyright"
            {
                if entry.file_type()?.is_dir() {
                    copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
                } else if entry.file_type()?.is_file() {
                    fs::copy(entry.path(), destination.join(entry.file_name()))?;
                }
            }
        }
    }
    fs::write(
        notices.join("dependencies.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let output = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()?;
    let sysroot = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    let rust_docs = sysroot.join("share/doc/rust");
    for name in ["COPYRIGHT", "LICENSE-APACHE", "LICENSE-MIT"] {
        if rust_docs.join(name).is_file() {
            fs::copy(rust_docs.join(name), notices.join(format!("Rust-{name}")))?;
        }
    }
    if rust_docs.join("licenses").is_dir() {
        copy_tree(&rust_docs.join("licenses"), &notices.join("Rust"))?;
    }
    Ok(())
}

pub async fn bundle() -> Result<PathBuf> {
    let root = crate::root();
    let target = host_target()?;
    let lock: Lock = serde_json::from_slice(
        &fs::read(root.join("vendor-lock.json"))
            .context("Run cargo xtask lock-artifacts first, then review the lock file.")?,
    )?;
    anyhow::ensure!(
        lock.browser_version == BROWSER_VERSION && lock.opencode_version == OPENCODE_VERSION,
        "Vendor lock versions do not match the build tooling."
    );
    let vendors = lock
        .targets
        .get(&target)
        .context("Target missing from vendor lock.")?;
    let rust_target = match target.as_str() {
        "macos-arm64" => "aarch64-apple-darwin",
        "macos-x64" => "x86_64-apple-darwin",
        "windows-x64" => "x86_64-pc-windows-msvc",
        "linux-x64" => "x86_64-unknown-linux-gnu",
        _ => bail!("Unsupported Rust build target."),
    };
    run(Command::new("cargo")
        .args([
            "build",
            "--release",
            "--locked",
            "--bin",
            "uva-opencode",
            "--target",
            rust_target,
            "--target-dir",
        ])
        .arg(root.join("target"))
        .current_dir(&root))?;
    let stage = root.join("dist/native/uva-opencode");
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir_all(&stage)?;
    let name = if cfg!(windows) {
        "uva-opencode.exe"
    } else {
        "uva-opencode"
    };
    fs::copy(
        root.join("target")
            .join(rust_target)
            .join("release")
            .join(name),
        stage.join(name),
    )?;
    let (browser, _) = download(
        &format!("browser-{target}.zip"),
        &vendors.browser.url,
        Some(&vendors.browser.sha256),
    )
    .await?;
    unzip(&browser, &stage.join("browsers"))?;
    let (opencode, _) = download(
        &format!("opencode-{target}.tgz"),
        &vendors.opencode.url,
        Some(&vendors.opencode.sha256),
    )
    .await?;
    extract_opencode(&opencode, &stage.join("vendor"))?;
    let (license, _) = download(
        "OpenCode-LICENSE.txt",
        &lock.license.url,
        Some(&lock.license.sha256),
    )
    .await?;
    fs::copy(license, stage.join("vendor/OpenCode-LICENSE.txt"))?;
    fs::copy(
        root.join("vendor-lock.json"),
        stage.join("vendor/provenance.json"),
    )?;
    fs::copy(
        root.join("packaging/RUST_THIRD_PARTY.md"),
        stage.join("THIRD_PARTY.md"),
    )?;
    notices(&stage)?;
    let state = tempfile::tempdir()?;
    run(Command::new(stage.join(name))
        .arg("--self-test")
        .env("UVA_STATE_DIR", state.path())
        // Ambient browser overrides must never let the bundle pass using an external runtime.
        .env(
            "UVA_BROWSER_PATH",
            stage.join("intentionally-missing-browser"),
        )
        .env(
            "UVA_BROWSERS_DIR",
            stage.join("intentionally-missing-browsers"),
        ))?;
    println!("Verified runtime bundle at {}", stage.display());
    Ok(stage)
}

pub async fn native() -> Result<()> {
    let stage = bundle().await?;
    let root = crate::root();
    let target = host_target()?;
    if cfg!(target_os = "macos") {
        let payload = root.join(".runtime/rust-package/pkgroot");
        if payload.exists() {
            fs::remove_dir_all(&payload)?;
        }
        copy_tree(
            &stage,
            &payload.join("Library/Application Support/UvAOpenCodeApp"),
        )?;
        let components = payload.parent().unwrap().join("components.plist");
        run(Command::new("pkgbuild")
            .arg("--analyze")
            .arg("--root")
            .arg(&payload)
            .arg(&components))?;
        let mut data = plist::Value::from_file(&components)?;
        fn nonrelocatable(value: &mut plist::Value) {
            match value {
                plist::Value::Dictionary(map) => {
                    if map.contains_key("BundleIsRelocatable") {
                        map.insert("BundleIsRelocatable".into(), plist::Value::Boolean(false));
                    }
                    for value in map.values_mut() {
                        nonrelocatable(value);
                    }
                }
                plist::Value::Array(values) => {
                    for value in values {
                        nonrelocatable(value);
                    }
                }
                _ => {}
            }
        }
        nonrelocatable(&mut data);
        data.to_file_xml(&components)?;
        run(Command::new("pkgbuild")
            .arg("--root")
            .arg(payload)
            .arg("--scripts")
            .arg(root.join("packaging/macos"))
            .arg("--component-plist")
            .arg(components)
            .args([
                "--identifier",
                "nl.uva.unofficial.uva-opencode",
                "--version",
                env!("CARGO_PKG_VERSION"),
            ])
            .arg(root.join(format!(
                "dist/uva-opencode-{}-{target}.pkg",
                env!("CARGO_PKG_VERSION")
            ))))?;
    } else if cfg!(windows) {
        let compiler = std::env::var_os("ISCC")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("ProgramFiles(x86)")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("C:/Program Files (x86)"))
                    .join("Inno Setup 6/ISCC.exe")
            });
        run(Command::new(compiler)
            .arg(format!("/DAppVersion={}", env!("CARGO_PKG_VERSION")))
            .arg(root.join("packaging/rust-windows.iss")))?;
    } else {
        let payload = tempfile::tempdir()?;
        let directory = payload.path().join("uva-opencode");
        copy_tree(&stage, &directory.join("app"))?;
        fs::copy(
            root.join("packaging/install-linux.sh"),
            directory.join("install.sh"),
        )?;
        executable(&directory.join("install.sh"), 0o755)?;
        let output = File::create(root.join(format!(
            "dist/uva-opencode-{}-{target}.tar.gz",
            env!("CARGO_PKG_VERSION")
        )))?;
        let mut archive = tar::Builder::new(GzEncoder::new(output, Compression::default()));
        archive.append_dir_all("uva-opencode", directory)?;
        archive.into_inner()?.finish()?;
    }
    println!(
        "Built native installer for {target}. Signing and publication are separate release steps."
    );
    Ok(())
}

pub fn source() -> Result<()> {
    let root = crate::root();
    fs::create_dir_all(root.join("dist"))?;
    let file = File::create(root.join("dist/uva-opencode-source.zip"))?;
    let mut archive = zip::ZipWriter::new(file);
    // No broad repository archive: credentials, runtime state, build products, and Python are excluded.
    for name in [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "README.md",
        "opencode.example.json",
        "vendor-lock.json",
        "rust-validation.json",
        "Open UvA OpenCode.command",
        "xtask/Cargo.toml",
        ".cargo/config.toml",
        "packaging/RUST_THIRD_PARTY.md",
        "packaging/rust-windows.iss",
        "packaging/install-linux.sh",
        "packaging/macos/postinstall",
    ] {
        let path = root.join(name);
        if path.is_file() {
            add_source(&mut archive, &root, &path)?;
        }
    }
    for name in ["src", "tests", "examples", "xtask/src"] {
        add_source(&mut archive, &root, &root.join(name))?;
    }
    add_source(
        &mut archive,
        &root,
        &root.join(".github/workflows/native-build.yml"),
    )?;
    archive.finish()?;
    println!("Created dist/uva-opencode-source.zip");
    Ok(())
}

pub async fn install(prefix: Option<PathBuf>) -> Result<()> {
    anyhow::ensure!(!cfg!(windows), "Use the Windows native installer.");
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("Cannot locate your home directory.")?;
    let default_app = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/UvAOpenCodeApp")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("uva-opencode-app")
    };
    let (app, launcher) = if let Some(prefix) = prefix {
        let prefix = if prefix.is_absolute() {
            prefix
        } else {
            std::env::current_dir()?.join(prefix)
        };
        (
            prefix.join("share/uva-opencode-app"),
            prefix.join("bin/uva-opencode"),
        )
    } else {
        (default_app, home.join(".local/bin/uva-opencode"))
    };
    // Inspect dangling symlinks too, and leave unrelated commands untouched.
    if let Ok(metadata) = fs::symlink_metadata(&launcher) {
        anyhow::ensure!(
            metadata.file_type().is_symlink()
                && fs::read_link(&launcher)? == app.join("uva-opencode"),
            "An existing uva-opencode command was left unchanged. Use the native bundle directly or remove the old command first."
        );
    }
    let stage = bundle().await?;
    let candidate = app.with_file_name("uva-opencode-app-next");
    if candidate.exists() {
        fs::remove_dir_all(&candidate)?;
    }
    copy_tree(&stage, &candidate)?;
    // Test the invocation layout used by real installations before replacing an older app.
    let verification = tempfile::tempdir()?;
    let linked = verification.path().join("uva-opencode");
    #[cfg(unix)]
    std::os::unix::fs::symlink(candidate.join("uva-opencode"), &linked)?;
    run(Command::new(&linked)
        .arg("--self-test")
        .env("UVA_STATE_DIR", verification.path().join("state"))
        .env(
            "UVA_BROWSER_PATH",
            verification.path().join("missing-browser"),
        )
        .env(
            "UVA_BROWSERS_DIR",
            verification.path().join("missing-browsers"),
        ))?;
    // Do not overwrite executables while an older helper uses its browser.
    let status = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::net::TcpStream::connect("127.0.0.1:8765"),
    )
    .await;
    anyhow::ensure!(
        matches!(status, Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused),
        "Port 8765 is active or could not be checked. Stop the helper before replacing its installation."
    );
    let previous = app.with_file_name("uva-opencode-app-previous");
    if previous.exists() {
        fs::remove_dir_all(&previous)?;
    }
    if app.exists() {
        fs::rename(&app, &previous)?;
    }
    if let Err(error) = fs::rename(&candidate, &app) {
        if previous.exists() {
            let _ = fs::rename(&previous, &app);
        }
        return Err(error.into());
    }
    fs::create_dir_all(launcher.parent().unwrap())?;
    if fs::symlink_metadata(&launcher).is_ok() {
        fs::remove_file(&launcher)?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(app.join("uva-opencode"), &launcher)?;
    println!(
        "Installed {}. Add its bin directory to PATH if needed.",
        launcher.display()
    );
    Ok(())
}

fn add_source(archive: &mut zip::ZipWriter<File>, root: &Path, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    anyhow::ensure!(
        !metadata.file_type().is_symlink(),
        "Source packages may not contain symlinks."
    );
    if metadata.is_dir() {
        let mut children = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            add_source(archive, root, &child.path())?;
        }
    } else {
        let relative = path.strip_prefix(root)?;
        let name = format!(
            "uva-opencode/{}",
            relative.to_string_lossy().replace('\\', "/")
        );
        let mode = if path
            .extension()
            .is_some_and(|e| e == "sh" || e == "command")
            || path.file_name().is_some_and(|n| n == "postinstall")
        {
            0o755
        } else {
            0o644
        };
        archive.start_file(
            name,
            zip::write::SimpleFileOptions::default().unix_permissions(mode),
        )?;
        std::io::copy(&mut File::open(path)?, archive)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extraction_rejects_traversal_and_absolute_paths() {
        for path in ["../cookies", "/private/profile", "chrome/../../profile"] {
            assert!(!safe_relative(Path::new(path)));
        }
        assert!(safe_relative(Path::new("chrome/Contents/MacOS/Chromium")));
    }
}
