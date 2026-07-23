use std::{
    collections::VecDeque,
    env,
    ffi::OsString,
    fs,
    io::{self, IsTerminal, Write},
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use tempfile::{Builder, TempDir};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, BufReader},
    process::{Child, Command},
    sync::mpsc,
    task::JoinHandle,
    time::Instant,
};

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(120);
const LOG_HISTORY: usize = 12;

pub enum ManagedTorMode {
    Client,
    OnionService { target: SocketAddr },
}

pub struct ManagedTor {
    pub proxy_address: String,
    pub onion_relay_url: Option<String>,
    child: Child,
    log_drains: Vec<JoinHandle<()>>,
    _workspace: TempDir,
}

impl ManagedTor {
    pub async fn start(binary: &Path, mode: ManagedTorMode) -> Result<Self> {
        let workspace = Builder::new()
            .prefix("airwire-tor-")
            .tempdir()
            .context("cannot create a temporary Tor workspace")?;
        let data_directory = workspace.path().join("data");
        let onion_directory = workspace.path().join("onion");
        let torrc = workspace.path().join("torrc");
        create_private_directory(&data_directory)?;
        fs::write(&torrc, b"").context("cannot create the temporary Tor configuration")?;

        let proxy_port = reserve_local_port()?;
        let proxy_address = format!("127.0.0.1:{proxy_port}");
        let mut command = Command::new(binary);
        command
            .arg("-f")
            .arg(&torrc)
            .arg("--DataDirectory")
            .arg(&data_directory)
            .arg("--SocksPort")
            .arg(&proxy_address)
            .arg("--ClientOnly")
            .arg("1")
            .arg("--SafeLogging")
            .arg("1")
            .arg("--Log")
            .arg("notice stdout")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let onion_hostname = match mode {
            ManagedTorMode::Client => None,
            ManagedTorMode::OnionService { target } => {
                create_private_directory(&onion_directory)?;
                command
                    .arg("--HiddenServiceDir")
                    .arg(&onion_directory)
                    .arg("--HiddenServiceVersion")
                    .arg("3")
                    .arg("--HiddenServicePort")
                    .arg(format!("80 {target}"));
                Some(onion_directory.join("hostname"))
            }
        };

        let mut child = command
            .spawn()
            .with_context(|| format!("cannot start Tor at {}", binary.display()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("cannot read Tor output"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("cannot read Tor errors"))?;
        let recent_logs = Arc::new(Mutex::new(VecDeque::with_capacity(LOG_HISTORY)));
        let (bootstrap_tx, mut bootstrap_rx) = mpsc::unbounded_channel();
        let log_drains = vec![
            tokio::spawn(drain_tor_output(
                stdout,
                bootstrap_tx.clone(),
                recent_logs.clone(),
            )),
            tokio::spawn(drain_tor_output(stderr, bootstrap_tx, recent_logs.clone())),
        ];

        println!("Starting an isolated Tor session…");
        let deadline = Instant::now() + BOOTSTRAP_TIMEOUT;
        let mut bootstrapped = false;
        let mut last_progress = None;
        loop {
            if let Some(status) = child
                .try_wait()
                .context("cannot inspect the managed Tor process")?
            {
                abort_tasks(&log_drains);
                bail!(
                    "Tor exited during startup ({status}){}",
                    format_recent_logs(&recent_logs)
                );
            }

            if bootstrapped
                && onion_hostname
                    .as_ref()
                    .is_none_or(|hostname| hostname.is_file())
            {
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill().await;
                abort_tasks(&log_drains);
                bail!(
                    "Tor did not finish connecting within {} seconds{}",
                    BOOTSTRAP_TIMEOUT.as_secs(),
                    format_recent_logs(&recent_logs)
                );
            }

            if let Ok(Some(progress)) =
                tokio::time::timeout(Duration::from_millis(250), bootstrap_rx.recv()).await
            {
                bootstrapped = progress == 100;
                if last_progress != Some(progress) {
                    println!("  Tor bootstrap: {progress}%");
                    last_progress = Some(progress);
                }
            }
        }

        let onion_relay_url = match onion_hostname {
            Some(hostname_file) => {
                let hostname = fs::read_to_string(&hostname_file)
                    .with_context(|| format!("cannot read {}", hostname_file.display()))?;
                let hostname = validate_onion_hostname(hostname.trim())?;
                Some(format!("ws://{hostname}/ws"))
            }
            None => None,
        };
        println!("Tor is ready.");

        Ok(Self {
            proxy_address,
            onion_relay_url,
            child,
            log_drains,
            _workspace: workspace,
        })
    }

    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
        abort_tasks(&self.log_drains);
    }
}

impl Drop for ManagedTor {
    fn drop(&mut self) {
        abort_tasks(&self.log_drains);
        let _ = self.child.start_kill();
    }
}

pub async fn ensure_tor_binary(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(configured) = configured {
        return resolve_executable(configured).ok_or_else(|| {
            anyhow!(
                "Tor executable not found at {}; correct --tor-binary or AIRWIRE_TOR_BINARY",
                configured.display()
            )
        });
    }
    if let Some(binary) = discover_tor_binary() {
        return Ok(binary);
    }
    if !io::stdin().is_terminal() {
        bail!("Tor is not installed or was not found; install Tor or set AIRWIRE_TOR_BINARY");
    }

    if let Some(installer) = platform_installer() {
        println!("Tor is required for automatic onion routing.");
        if confirm(&format!(
            "Install it now with `{}`? [Y/n] ",
            installer.label
        ))? {
            let status = Command::new(&installer.program)
                .args(&installer.arguments)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await
                .with_context(|| format!("cannot run {}", installer.label))?;
            if !status.success() {
                bail!("Tor installation failed with status {status}");
            }
            if let Some(binary) = discover_tor_binary() {
                return Ok(binary);
            }
        }
    }

    let path = prompt("Path to the Tor executable (leave blank to cancel): ")?;
    if path.is_empty() {
        bail!("Tor setup cancelled");
    }
    resolve_executable(Path::new(&path)).ok_or_else(|| {
        anyhow!("Tor executable not found at {path}; set AIRWIRE_TOR_BINARY to its full path")
    })
}

fn discover_tor_binary() -> Option<PathBuf> {
    find_on_path("tor").or_else(find_common_tor_binary)
}

fn resolve_executable(value: &Path) -> Option<PathBuf> {
    if value.components().count() == 1 {
        value
            .to_str()
            .and_then(find_on_path)
            .or_else(|| executable_file(value))
    } else {
        executable_file(value)
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for directory in env::split_paths(&path) {
        for candidate_name in executable_names(name) {
            if let Some(candidate) = executable_file(&directory.join(candidate_name)) {
                return Some(candidate);
            }
        }
    }
    None
}

fn executable_names(name: &str) -> Vec<OsString> {
    #[cfg(windows)]
    {
        if Path::new(name).extension().is_some() {
            vec![OsString::from(name)]
        } else {
            vec![OsString::from(format!("{name}.exe")), OsString::from(name)]
        }
    }
    #[cfg(not(windows))]
    {
        vec![OsString::from(name)]
    }
}

fn executable_file(path: &Path) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.metadata().ok()?.permissions().mode() & 0o111 == 0 {
            return None;
        }
    }
    Some(path.to_path_buf())
}

fn find_common_tor_binary() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(target_os = "macos")]
    {
        candidates.extend([
            PathBuf::from("/opt/homebrew/bin/tor"),
            PathBuf::from("/usr/local/bin/tor"),
            PathBuf::from("/opt/local/bin/tor"),
        ]);
    }
    #[cfg(unix)]
    {
        candidates.extend([
            PathBuf::from("/usr/bin/tor"),
            PathBuf::from("/usr/local/bin/tor"),
        ]);
    }
    #[cfg(windows)]
    {
        if let Some(directory) = env::var_os("LOCALAPPDATA") {
            candidates.push(PathBuf::from(directory).join("Airwire\\tor-expert\\tor\\tor.exe"));
        }
        for variable in ["LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"] {
            if let Some(directory) = env::var_os(variable) {
                candidates.push(
                    PathBuf::from(directory).join("Tor Browser\\Browser\\TorBrowser\\Tor\\tor.exe"),
                );
            }
        }
        if let Some(profile) = env::var_os("USERPROFILE") {
            for directory in ["Desktop", "Downloads"] {
                candidates.push(
                    PathBuf::from(&profile)
                        .join(directory)
                        .join("Tor Browser\\Browser\\TorBrowser\\Tor\\tor.exe"),
                );
            }
        }
    }
    candidates
        .into_iter()
        .find_map(|candidate| executable_file(&candidate))
}

struct Installer {
    program: PathBuf,
    arguments: Vec<OsString>,
    label: String,
}

#[cfg(target_os = "macos")]
fn platform_installer() -> Option<Installer> {
    let brew = find_on_path("brew")?;
    Some(Installer {
        program: brew,
        arguments: vec![OsString::from("install"), OsString::from("tor")],
        label: "brew install tor".into(),
    })
}

#[cfg(target_os = "linux")]
fn platform_installer() -> Option<Installer> {
    let choices: [(&str, &[&str]); 5] = [
        ("apt-get", &["install", "--yes", "tor"]),
        ("dnf", &["install", "--assumeyes", "tor"]),
        ("pacman", &["--sync", "--needed", "--noconfirm", "tor"]),
        ("zypper", &["--non-interactive", "install", "tor"]),
        ("apk", &["add", "tor"]),
    ];
    for (manager, arguments) in choices {
        if let Some(manager_path) = find_on_path(manager) {
            let mut command_arguments: Vec<OsString> =
                arguments.iter().map(OsString::from).collect();
            let label = format!("{manager} {}", arguments.join(" "));
            if let Some(sudo) = find_on_path("sudo") {
                command_arguments.insert(0, manager_path.into_os_string());
                return Some(Installer {
                    program: sudo,
                    arguments: command_arguments,
                    label: format!("sudo {label}"),
                });
            }
            return Some(Installer {
                program: manager_path,
                arguments: command_arguments,
                label,
            });
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_installer() -> Option<Installer> {
    None
}

fn confirm(question: &str) -> Result<bool> {
    let answer = prompt(question)?;
    Ok(answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

fn prompt(question: &str) -> Result<String> {
    print!("{question}");
    io::stdout().flush().context("cannot display Tor prompt")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .context("cannot read Tor setup response")?;
    Ok(answer.trim().to_owned())
}

fn reserve_local_port() -> Result<u16> {
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).context("cannot reserve a local Tor SOCKS port")?;
    Ok(listener
        .local_addr()
        .context("cannot inspect the local Tor SOCKS port")?
        .port())
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot secure {}", path.display()))?;
    }
    Ok(())
}

async fn drain_tor_output<R>(
    reader: R,
    bootstrap: mpsc::UnboundedSender<u8>,
    recent_logs: Arc<Mutex<VecDeque<String>>>,
) where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(progress) = parse_bootstrap_percent(&line) {
            let _ = bootstrap.send(progress);
        }
        if let Ok(mut logs) = recent_logs.lock() {
            if logs.len() == LOG_HISTORY {
                logs.pop_front();
            }
            logs.push_back(line);
        }
    }
}

fn parse_bootstrap_percent(line: &str) -> Option<u8> {
    let remainder = line.split_once("Bootstrapped ")?.1;
    let percentage = remainder.split_once('%')?.0;
    percentage.parse().ok().filter(|value| *value <= 100)
}

fn validate_onion_hostname(hostname: &str) -> Result<&str> {
    if !is_valid_v3_onion_hostname(hostname) {
        bail!("Tor returned an invalid v3 onion hostname");
    }
    Ok(hostname)
}

pub(crate) fn is_valid_v3_onion_hostname(hostname: &str) -> bool {
    hostname.strip_suffix(".onion").is_some_and(|service_id| {
        service_id.len() == 56
            && service_id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
    })
}

fn format_recent_logs(logs: &Arc<Mutex<VecDeque<String>>>) -> String {
    let Ok(logs) = logs.lock() else {
        return String::new();
    };
    if logs.is_empty() {
        String::new()
    } else {
        format!(
            "\nRecent Tor output:\n{}",
            logs.iter().cloned().collect::<Vec<_>>().join("\n")
        )
    }
}

fn abort_tasks(tasks: &[JoinHandle<()>]) {
    for task in tasks {
        task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bootstrap_progress() {
        assert_eq!(
            parse_bootstrap_percent(
                "Jul 23 17:00:00 [notice] Bootstrapped 80% (ap_conn_done): Connected"
            ),
            Some(80)
        );
        assert_eq!(parse_bootstrap_percent("unrelated output"), None);
    }

    #[test]
    fn validates_v3_onion_hostname() {
        let hostname = format!("{}.onion", "a".repeat(56));
        assert_eq!(validate_onion_hostname(&hostname).unwrap(), hostname);
        assert!(validate_onion_hostname("short.onion").is_err());
        assert!(validate_onion_hostname(&format!("{}.onion", "1".repeat(56))).is_err());
    }
}
