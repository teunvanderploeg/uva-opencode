use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Paths {
    pub state: PathBuf,
    pub app: PathBuf,
    pub browsers: PathBuf,
}

impl Paths {
    pub fn discover() -> Result<Self> {
        let app = dunce::canonicalize(env::current_exe()?)?
            .parent()
            .context("Executable has no parent directory")?
            .to_path_buf();
        let source = dunce::canonicalize(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
        let development = app.starts_with(source.join("target"));
        let home = home()?;
        let default_state = if development {
            source.join(".runtime/rust")
        } else if cfg!(target_os = "macos") {
            home.join("Library/Application Support/UvAOpenCode")
        } else if cfg!(windows) {
            env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData/Local"))
                .join("UvAOpenCode")
        } else {
            env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/share"))
                .join("uva-opencode")
        };
        let state = env::var_os("UVA_STATE_DIR")
            .map(PathBuf::from)
            .unwrap_or(default_state);
        let browsers = env::var_os("UVA_BROWSERS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if development {
                    source.join(".runtime/browsers")
                } else {
                    app.join("browsers")
                }
            });
        Ok(Self {
            state: std::path::absolute(state)?,
            app,
            browsers: std::path::absolute(browsers)?,
        })
    }

    pub fn browser_executable(&self) -> Result<PathBuf> {
        if let Some(path) = env::var_os("UVA_BROWSER_PATH") {
            let path = PathBuf::from(path);
            anyhow::ensure!(
                path.is_file(),
                "UVA_BROWSER_PATH does not point to a browser executable."
            );
            return Ok(path);
        }
        Self::find_browser(&self.browsers)
    }

    pub fn bundled_browser_executable(&self) -> Result<PathBuf> {
        Self::find_browser(&self.app.join("browsers"))
            .context("The installed Chromium bundle is missing or incomplete.")
    }

    fn find_browser(directory: &Path) -> Result<PathBuf> {
        let mut roots = vec![directory.to_path_buf()];
        if let Ok(entries) = fs::read_dir(directory) {
            roots.extend(entries.filter_map(|entry| entry.ok().map(|e| e.path())));
        }
        roots.sort();
        for root in roots.into_iter().rev() {
            for relative in [
                "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
                "chrome-mac/Chromium.app/Contents/MacOS/Chromium",
                "chrome-linux64/chrome",
                "chrome-linux/chrome",
                "chrome-win64/chrome.exe",
                "chrome-win/chrome.exe",
            ] {
                let path = root.join(relative);
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
        bail!("Bundled Chromium was not found. Set UVA_BROWSER_PATH or install a native package.")
    }
}

pub fn home() -> Result<PathBuf> {
    env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .context("Cannot find your home directory.")
}

pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        // Remove inherited access and grant only the current Windows account.
        // Use argv directly; account and path strings never pass through a shell.
        let user = env::var("USERNAME").context("Cannot determine the Windows account.")?;
        let domain = env::var("USERDOMAIN").unwrap_or_default();
        let account = if domain.is_empty() {
            user
        } else {
            format!("{domain}\\{user}")
        };
        let result = std::process::Command::new("icacls")
            .arg(path)
            .args([
                "/inheritance:r",
                "/grant:r",
                &format!("{account}:(OI)(CI)F"),
            ])
            .output()?;
        anyhow::ensure!(
            result.status.success(),
            "Could not restrict access to private application data."
        );
    }
    Ok(())
}

pub fn private_write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents.as_ref())?;
    Ok(())
}
