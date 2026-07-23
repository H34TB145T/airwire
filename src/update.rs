use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, bail};

const REPOSITORY: &str = "H34TB145T/airwire";
const INSTALLER_BASE: &str = "https://raw.githubusercontent.com/H34TB145T/airwire/main";

pub fn start() -> Result<()> {
    let executable = env::current_exe().context("cannot locate the running Airwire executable")?;
    let install_directory = executable
        .parent()
        .context("the running Airwire executable has no parent directory")?;

    #[cfg(windows)]
    launch_windows(install_directory)?;

    #[cfg(unix)]
    launch_unix(install_directory)?;

    #[cfg(not(any(windows, unix)))]
    bail!("automatic updates are not supported on this platform");

    println!("Airwire updater started for {}.", executable.display());
    println!("Keep this terminal open; it will report when the update is complete.");
    Ok(())
}

fn helper_path(extension: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "airwire-update-{}-{}.{}",
        std::process::id(),
        rand::random::<u64>(),
        extension
    ))
}

#[cfg(windows)]
fn launch_windows(install_directory: &Path) -> Result<()> {
    let helper = helper_path("ps1");
    let installer = helper.with_extension("installer.ps1");
    fs::write(
        &helper,
        windows_script(std::process::id(), install_directory, &helper, &installer),
    )
    .with_context(|| format!("cannot create update helper at {}", helper.display()))?;

    let spawn_result = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&helper)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();
    if let Err(error) = spawn_result {
        let _ = fs::remove_file(&helper);
        bail!("cannot start the Windows updater: {error}");
    }
    Ok(())
}

#[cfg(unix)]
fn launch_unix(install_directory: &Path) -> Result<()> {
    let helper = helper_path("sh");
    let installer = helper.with_extension("installer.sh");
    fs::write(
        &helper,
        unix_script(std::process::id(), install_directory, &helper, &installer),
    )
    .with_context(|| format!("cannot create update helper at {}", helper.display()))?;

    let spawn_result = Command::new("/bin/sh")
        .arg(&helper)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();
    if let Err(error) = spawn_result {
        let _ = fs::remove_file(&helper);
        bail!("cannot start the updater: {error}");
    }
    Ok(())
}

#[cfg(any(windows, test))]
fn windows_script(
    parent_pid: u32,
    install_directory: &Path,
    helper: &Path,
    installer: &Path,
) -> String {
    let install_directory = powershell_quote(install_directory);
    let helper = powershell_quote(helper);
    let installer = powershell_quote(installer);
    format!(
        r#"$ErrorActionPreference = "Stop"
$InstallerPath = '{installer}'
try {{
    Wait-Process -Id {parent_pid} -ErrorAction SilentlyContinue
    $env:AIRWIRE_REPOSITORY = "{REPOSITORY}"
    $env:AIRWIRE_VERSION = "latest"
    $env:AIRWIRE_INSTALL_DIR = '{install_directory}'
    Remove-Item Env:AIRWIRE_DOWNLOAD_BASE -ErrorAction SilentlyContinue
    Invoke-WebRequest -Uri "{INSTALLER_BASE}/install.ps1" -OutFile $InstallerPath
    & $InstallerPath
    Write-Host "Airwire update complete."
}} catch {{
    Write-Error $_
}} finally {{
    Remove-Item -LiteralPath $InstallerPath -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath '{helper}' -Force -ErrorAction SilentlyContinue
}}
"#
    )
}

#[cfg(any(unix, test))]
fn unix_script(
    parent_pid: u32,
    install_directory: &Path,
    helper: &Path,
    installer: &Path,
) -> String {
    let install_directory = shell_quote(install_directory);
    let helper = shell_quote(helper);
    let installer = shell_quote(installer);
    format!(
        r#"#!/bin/sh
set -eu
while kill -0 {parent_pid} 2>/dev/null; do
    sleep 1
done
helper={helper}
installer={installer}
trap 'rm -f "$installer" "$helper"' EXIT HUP INT TERM
if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error "{INSTALLER_BASE}/install.sh" --output "$installer"
elif command -v wget >/dev/null 2>&1; then
    wget --quiet "{INSTALLER_BASE}/install.sh" --output-document="$installer"
else
    printf 'airwire updater: curl or wget is required\n' >&2
    exit 1
fi
unset AIRWIRE_DOWNLOAD_BASE
AIRWIRE_REPOSITORY="{REPOSITORY}" AIRWIRE_VERSION="latest" AIRWIRE_INSTALL_DIR={install_directory} AIRWIRE_NO_PATH_UPDATE="1" /bin/sh "$installer"
printf 'Airwire update complete.\n'
"#
    )
}

#[cfg(any(windows, test))]
fn powershell_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

#[cfg(any(unix, test))]
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_helper_waits_and_updates_the_running_executable_directory() {
        let script = windows_script(
            42,
            Path::new(r"C:\Users\O'Brien\Airwire\bin"),
            Path::new(r"C:\Temp\helper.ps1"),
            Path::new(r"C:\Temp\installer.ps1"),
        );

        assert!(script.contains("Wait-Process -Id 42"));
        assert!(script.contains(r"C:\Users\O''Brien\Airwire\bin"));
        assert!(script.contains("AIRWIRE_VERSION = \"latest\""));
        assert!(script.contains("install.ps1"));
    }

    #[test]
    fn unix_helper_waits_and_quotes_the_running_executable_directory() {
        let script = unix_script(
            42,
            Path::new("/tmp/O'Brien/bin"),
            Path::new("/tmp/helper.sh"),
            Path::new("/tmp/installer.sh"),
        );

        assert!(script.contains("kill -0 42"));
        assert!(script.contains("AIRWIRE_VERSION=\"latest\""));
        assert!(script.contains("AIRWIRE_NO_PATH_UPDATE=\"1\""));
        assert!(script.contains("'/tmp/O'\"'\"'Brien/bin'"));
        assert!(script.contains("install.sh"));
    }
}
