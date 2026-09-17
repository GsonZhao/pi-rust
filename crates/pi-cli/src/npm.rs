//! Native Pi-compatible npm command selection and argument construction.
//!
//! `npmCommand` is an argv array, not a shell command. The first entry is the
//! executable and every remaining entry is a fixed prefix prepended to npm
//! operations. This permits wrappers such as
//! `["mise", "exec", "node@20", "--", "npm"]` without shell parsing.

use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use tokio::io::AsyncReadExt;

#[cfg(unix)]
use std::process::Stdio;

use crate::settings::Settings;

const NPM_PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_NPM_PROCESS_OUTPUT_BYTES: usize = 64 * 1024;
const NPM_STARTUP_REMEDIATION_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_NPM_STARTUP_REMEDIATION_OUTPUT_BYTES: usize = 1024 * 1024;
const NPM_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const NPM_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NpmProcessCwd {
    /// Run outside the caller's working tree so project-local npm config is
    /// not consulted before that project has passed its trust gate.
    Isolated,
    /// Use a project directory only after the caller has established trust.
    Trusted(PathBuf),
}

impl NpmProcessCwd {
    pub(crate) fn startup(trusted_project_cwd: Option<&Path>) -> Self {
        trusted_project_cwd
            .map(|path| Self::Trusted(path.to_path_buf()))
            .unwrap_or(Self::Isolated)
    }
}

#[derive(Debug)]
pub(crate) struct NpmProcessOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NpmProcessError {
    Setup(String),
    Spawn(String),
    Read(String),
    Wait(String),
    Cancelled,
    TimedOut,
    OutputLimitExceeded { limit: usize },
}

impl fmt::Display for NpmProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "could not prepare npm process: {error}"),
            Self::Spawn(error) => write!(formatter, "could not start npm process: {error}"),
            Self::Read(error) => write!(formatter, "could not read npm process output: {error}"),
            Self::Wait(error) => write!(formatter, "could not wait for npm process: {error}"),
            Self::Cancelled => write!(formatter, "npm process was cancelled"),
            Self::TimedOut => write!(formatter, "npm process timed out"),
            Self::OutputLimitExceeded { limit } => {
                write!(
                    formatter,
                    "npm process exceeded the {limit} byte output limit"
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct NpmProcessLimits {
    timeout: Duration,
    max_output_bytes: usize,
}

impl Default for NpmProcessLimits {
    fn default() -> Self {
        Self {
            timeout: NPM_PROCESS_TIMEOUT,
            max_output_bytes: MAX_NPM_PROCESS_OUTPUT_BYTES,
        }
    }
}

impl NpmProcessLimits {
    fn startup_remediation() -> Self {
        Self {
            timeout: NPM_STARTUP_REMEDIATION_TIMEOUT,
            max_output_bytes: MAX_NPM_STARTUP_REMEDIATION_OUTPUT_BYTES,
        }
    }
}

/// Package-manager behavior selected from the effective `npmCommand` argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpmManagerKind {
    Npm,
    Pnpm,
    Bun,
    Other,
}

/// A validated package-manager executable plus its fixed argument prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmCommand {
    program: String,
    prefix_args: Vec<String>,
    manager_kind: NpmManagerKind,
    configured: bool,
}

impl NpmCommand {
    /// Resolve the effective command using `.rpi`, `.pi`, global, then npm.
    /// Project settings are considered only after the project trust gate has
    /// passed because `npmCommand` can name an arbitrary executable.
    pub fn resolve(cwd: &Path, project_trusted: bool) -> Result<Self, String> {
        let project_settings = if project_trusted {
            crate::settings::load_project_settings(cwd)
        } else {
            Vec::new()
        };
        let global_settings = crate::settings::load_settings()
            .map_err(|error| format!("could not load npmCommand settings: {error}"))?;
        Self::from_argv(select_argv(
            &project_settings,
            &global_settings,
            project_trusted,
        ))
    }

    /// Build a command from Pi's optional argv setting. A missing or empty
    /// array selects the platform npm launcher. A blank executable is invalid
    /// and never falls back to another command.
    pub fn from_argv(argv: Option<&[String]>) -> Result<Self, String> {
        let Some(argv) = argv.filter(|argv| !argv.is_empty()) else {
            let program = default_npm_program().to_string();
            return Ok(Self {
                manager_kind: manager_kind(&program, &[]),
                program,
                prefix_args: Vec::new(),
                configured: false,
            });
        };
        if argv[0].trim().is_empty() {
            return Err(
                "invalid npmCommand: first array entry must be a non-empty command".to_string(),
            );
        }
        let program = windows_command_program(&argv[0]);
        let prefix_args = argv[1..].to_vec();
        Ok(Self {
            manager_kind: manager_kind(&program, &prefix_args),
            program,
            prefix_args,
            configured: true,
        })
    }

    /// Executable launched as an argv command. Windows `.cmd`/`.bat` shims
    /// use the standard library's hardened batch argument encoding.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Fixed arguments configured before each package-manager operation.
    pub fn prefix_args(&self) -> &[String] {
        &self.prefix_args
    }

    /// Whether the effective command came from an explicit `npmCommand`.
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// Package-manager family used for manager-specific install flags.
    pub fn manager_kind(&self) -> NpmManagerKind {
        self.manager_kind
    }

    /// Prepend the fixed configured argv to one operation's arguments.
    pub fn combined_args(&self, operation_args: &[String]) -> Vec<String> {
        let mut args = Vec::with_capacity(self.prefix_args.len() + operation_args.len());
        args.extend(self.prefix_args.iter().cloned());
        args.extend(operation_args.iter().cloned());
        args
    }

    /// Construct the complete argv for native Pi's registry version lookup.
    pub fn view_args(&self, source_spec: &str) -> Result<Vec<String>, String> {
        let spec = source_spec
            .strip_prefix("npm:")
            .unwrap_or(source_spec)
            .trim();
        if spec.is_empty() || spec.starts_with('-') || spec.chars().any(char::is_control) {
            return Err("npm package spec must be a safe, non-empty argument".to_string());
        }
        Ok(self.combined_args(&[
            "view".to_string(),
            spec.to_string(),
            "version".to_string(),
            "--json".to_string(),
        ]))
    }

    /// Construct manager-specific operation args for a managed npm root.
    /// These match native Pi's `getNpmInstallArgs` ordering exactly.
    pub fn install_args(&self, specs: &[String], install_root: &Path) -> Vec<String> {
        let root = install_root.to_string_lossy().into_owned();
        match self.manager_kind {
            NpmManagerKind::Bun => {
                let mut args = vec!["install".to_string()];
                args.extend(specs.iter().cloned());
                args.extend(["--cwd".to_string(), root, "--omit=peer".to_string()]);
                args
            }
            NpmManagerKind::Pnpm => {
                let mut args = vec!["install".to_string()];
                args.extend(specs.iter().cloned());
                args.extend([
                    "--prefix".to_string(),
                    root,
                    "--config.auto-install-peers=false".to_string(),
                    "--config.strict-peer-dependencies=false".to_string(),
                    "--config.strict-dep-builds=false".to_string(),
                ]);
                args
            }
            NpmManagerKind::Npm | NpmManagerKind::Other => {
                let mut args = vec!["install".to_string()];
                args.extend(specs.iter().cloned());
                args.extend([
                    "--prefix".to_string(),
                    root,
                    "--legacy-peer-deps".to_string(),
                ]);
                args
            }
        }
    }

    /// Construct manager-specific arguments for removing one package from a
    /// managed npm root. The ordering and flags match native Pi's
    /// `uninstallNpm` implementation.
    pub fn uninstall_args(&self, package_name: &str, install_root: &Path) -> Vec<String> {
        let root = install_root.to_string_lossy().into_owned();
        match self.manager_kind {
            NpmManagerKind::Bun => vec![
                "uninstall".to_string(),
                package_name.to_string(),
                "--cwd".to_string(),
                root,
            ],
            NpmManagerKind::Pnpm => vec![
                "uninstall".to_string(),
                package_name.to_string(),
                "--prefix".to_string(),
                root,
            ],
            NpmManagerKind::Npm | NpmManagerKind::Other => vec![
                "uninstall".to_string(),
                package_name.to_string(),
                "--prefix".to_string(),
                root,
                "--legacy-peer-deps".to_string(),
            ],
        }
    }

    /// Resolve the package-manager's legacy global install root(s).
    ///
    /// Native Pi uses the configured `npmCommand` for this lookup. The result
    /// is intentionally read-only metadata: callers may use it to discover a
    /// package that predates Pi's managed store, but must not use it as an
    /// rpi-owned install or removal root. Every returned path is absolute,
    /// canonical, an existing directory, and ends in `node_modules`.
    pub fn global_package_roots(&self) -> Result<Vec<PathBuf>, String> {
        match self.manager_kind {
            // Bun exposes the global binary directory rather than a node_modules
            // root. This is the same derivation used by native Pi.
            NpmManagerKind::Bun => {
                let output = self.capture_output(&["pm", "bin", "-g"])?;
                let bin_dir = parse_single_absolute_line(&output).ok_or_else(|| {
                    format!(
                        "{} pm bin -g returned an empty or non-absolute path",
                        self.program
                    )
                })?;
                let parent = bin_dir.parent().ok_or_else(|| {
                    format!(
                        "{} pm bin -g returned a path without a parent",
                        self.program
                    )
                })?;
                let candidate = parent.join("install").join("global").join("node_modules");
                validate_global_root(&candidate)
                    .map(|root| vec![root])
                    .ok_or_else(|| {
                        format!(
                            "{} pm bin -g did not resolve to a valid global node_modules path: {}",
                            self.program,
                            candidate.display()
                        )
                    })
            }
            // npm, pnpm, and compatible wrappers expose `root -g`. For pnpm
            // package paths themselves we additionally consult `list -g`
            // below, because pnpm's virtual store is nested below this root.
            NpmManagerKind::Npm | NpmManagerKind::Pnpm | NpmManagerKind::Other => {
                let output = self.capture_output(&["root", "-g"])?;
                let line = parse_single_absolute_line(&output).ok_or_else(|| {
                    format!(
                        "{} root -g returned an empty or non-absolute path",
                        self.program
                    )
                })?;
                validate_global_root(&line)
                    .map(|root| vec![root])
                    .ok_or_else(|| {
                        format!(
                            "{} root -g returned an invalid global node_modules path: {}",
                            self.program,
                            line.display()
                        )
                    })
            }
        }
    }

    /// Locate packages in a legacy global install with one package-manager
    /// query for the whole batch.
    ///
    /// npm/bun use `<global root>/<package name>`. pnpm reports the concrete
    /// virtual-store path through `list -g --depth 0 --json`, so that output
    /// is parsed instead of guessing a symlink layout. Returned paths are
    /// canonical read-only discovery paths; package updates must migrate into
    /// the rpi/native managed root before writing anything.
    pub fn global_package_paths(
        &self,
        package_names: &[String],
    ) -> Result<std::collections::HashMap<String, PathBuf>, String> {
        for package_name in package_names {
            if !valid_package_name(package_name) {
                return Err(format!(
                    "invalid npm package name for global lookup: {package_name}"
                ));
            }
        }
        if package_names.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        if self.manager_kind == NpmManagerKind::Pnpm {
            let output = self.capture_output(&["list", "-g", "--depth", "0", "--json"])?;
            return Ok(parse_pnpm_global_package_paths(&output, package_names));
        }

        let roots = self.global_package_roots()?;
        Ok(package_names
            .iter()
            .filter_map(|package_name| {
                roots
                    .iter()
                    .find_map(|root| validate_global_package_path(root, package_name))
                    .map(|path| (package_name.clone(), path))
            })
            .collect())
    }

    /// Locate one package using the same validation and bounded lookup as the
    /// batch API.
    pub fn global_package_path(&self, package_name: &str) -> Result<Option<PathBuf>, String> {
        let package_name = package_name.to_string();
        Ok(self
            .global_package_paths(std::slice::from_ref(&package_name))?
            .remove(&package_name))
    }

    /// Validate a command output path without executing a package manager.
    /// This is useful to callers that cache global-root discovery and to tests;
    /// it does not grant write or delete authority.
    pub fn validate_global_package_path(root: &Path, package_name: &str) -> Option<PathBuf> {
        validate_global_package_path(root, package_name)
    }

    /// Execute one npm operation without user-controlled shell parsing, with a
    /// hard deadline and a combined stdout/stderr byte budget. Cancellation
    /// always terminates and waits for the child tree before returning.
    pub(crate) async fn run_bounded(
        &self,
        args: &[String],
        cwd: NpmProcessCwd,
    ) -> Result<NpmProcessOutput, NpmProcessError> {
        run_bounded_process(&self.program, args, cwd, NpmProcessLimits::default()).await
    }

    /// Run an npm operation synchronously during startup package remediation.
    /// This uses a dedicated runtime thread because startup resolution is a
    /// synchronous compatibility boundary that can be called from Tokio. The
    /// longer install budget remains finite, captures bounded diagnostics, and
    /// reuses the same process-group/Job Object teardown as registry lookups.
    pub(crate) fn run_startup_remediation(
        &self,
        operation_args: &[String],
        cwd: &Path,
    ) -> Result<(), String> {
        let output = self
            .run_startup_remediation_with_limits(
                operation_args,
                cwd,
                NpmProcessLimits::startup_remediation(),
            )
            .map_err(|error| {
                format!("{} startup package operation failed: {error}", self.program)
            })?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let detail = if stderr.is_empty() {
            format!("exited with {}", output.status)
        } else {
            format!(
                "exited with {}: {}",
                output.status,
                truncate_output(&stderr)
            )
        };
        Err(format!("{} {detail}", self.program))
    }

    fn run_startup_remediation_with_limits(
        &self,
        operation_args: &[String],
        cwd: &Path,
        limits: NpmProcessLimits,
    ) -> Result<NpmProcessOutput, NpmProcessError> {
        let args = self.combined_args(operation_args);
        run_bounded_process_blocking(
            &self.program,
            &args,
            NpmProcessCwd::Trusted(cwd.to_path_buf()),
            limits,
            "rpi-npm-startup-remediation",
        )
    }

    fn capture_output(&self, operation_args: &[&str]) -> Result<String, String> {
        let operation_args = operation_args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>();
        let args = self.combined_args(&operation_args);
        let output = run_bounded_process_blocking(
            &self.program,
            &args,
            NpmProcessCwd::Isolated,
            NpmProcessLimits::default(),
            "rpi-npm-global-lookup",
        )
        .map_err(|error| {
            format!(
                "could not execute {} for global package lookup: {error}",
                self.program
            )
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("exited with {}", output.status)
            } else {
                // Keep diagnostics bounded; package-manager failures can dump
                // a very large log and this path is only a best-effort lookup.
                format!(
                    "exited with {}: {}",
                    output.status,
                    truncate_output(&stderr)
                )
            };
            return Err(format!("{} {}", self.program, detail));
        }
        String::from_utf8(output.stdout).map_err(|error| {
            format!(
                "{} global package lookup returned non-UTF-8 output: {error}",
                self.program
            )
        })
    }
}

fn run_bounded_process_blocking(
    program: &str,
    args: &[String],
    cwd: NpmProcessCwd,
    limits: NpmProcessLimits,
    thread_name: &str,
) -> Result<NpmProcessOutput, NpmProcessError> {
    run_bounded_process_blocking_with_environment(
        program,
        args,
        cwd,
        limits,
        Vec::new(),
        thread_name,
    )
}

/// Synchronous counterpart to [`run_bounded_command`]. The process runs on a
/// dedicated Tokio runtime thread, so callers may safely use this at a sync
/// startup boundary even when that boundary is entered from a Tokio runtime.
pub(crate) fn run_bounded_command_blocking(
    program: &str,
    args: &[String],
    cwd: &Path,
    environment: &[(OsString, Option<OsString>)],
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<NpmProcessOutput, NpmProcessError> {
    run_bounded_process_blocking_with_environment(
        program,
        args,
        NpmProcessCwd::Trusted(cwd.to_path_buf()),
        NpmProcessLimits {
            timeout,
            max_output_bytes,
        },
        environment.to_vec(),
        "rpi-bounded-startup-command",
    )
}

fn run_bounded_process_blocking_with_environment(
    program: &str,
    args: &[String],
    cwd: NpmProcessCwd,
    limits: NpmProcessLimits,
    environment: Vec<(OsString, Option<OsString>)>,
    thread_name: &str,
) -> Result<NpmProcessOutput, NpmProcessError> {
    let program = program.to_string();
    let args = args.to_vec();
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| NpmProcessError::Setup(error.to_string()))?;
            runtime.block_on(run_bounded_process_inner(
                &program,
                &args,
                cwd,
                limits,
                environment,
                None,
            ))
        })
        .map_err(|error| NpmProcessError::Setup(error.to_string()))?
        .join()
        .map_err(|_| NpmProcessError::Setup("npm process worker panicked".to_string()))?
}

async fn run_bounded_process(
    program: &str,
    args: &[String],
    cwd: NpmProcessCwd,
    limits: NpmProcessLimits,
) -> Result<NpmProcessOutput, NpmProcessError> {
    run_bounded_process_with_environment(program, args, cwd, limits, Vec::new()).await
}

/// Run a command with the same bounded output and whole-process-tree cleanup
/// used by npm startup work. Environment entries with `Some(value)` are set;
/// entries with `None` are removed from the inherited environment.
pub(crate) async fn run_bounded_command(
    program: &str,
    args: &[String],
    cwd: &Path,
    environment: &[(OsString, Option<OsString>)],
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<NpmProcessOutput, NpmProcessError> {
    run_bounded_process_with_environment(
        program,
        args,
        NpmProcessCwd::Trusted(cwd.to_path_buf()),
        NpmProcessLimits {
            timeout,
            max_output_bytes,
        },
        environment.to_vec(),
    )
    .await
}

async fn run_bounded_process_with_environment(
    program: &str,
    args: &[String],
    cwd: NpmProcessCwd,
    limits: NpmProcessLimits,
    environment: Vec<(OsString, Option<OsString>)>,
) -> Result<NpmProcessOutput, NpmProcessError> {
    let (cancel_tx, cancel_rx) = std::sync::mpsc::channel();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let program = program.to_string();
    let args = args.to_vec();
    let supervisor = std::thread::Builder::new()
        .name("rpi-npm-process".to_string())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| NpmProcessError::Setup(error.to_string()))
                .and_then(|runtime| {
                    runtime.block_on(run_bounded_process_inner(
                        &program,
                        &args,
                        cwd,
                        limits,
                        environment,
                        Some(cancel_rx),
                    ))
                });
            let _ = result_tx.send(result);
        })
        .map_err(|error| NpmProcessError::Setup(error.to_string()))?;
    let mut supervisor = NpmProcessSupervisor {
        cancel: Some(cancel_tx),
        thread: Some(supervisor),
    };

    let result = result_rx.await.map_err(|_| {
        NpmProcessError::Setup("npm process supervisor stopped unexpectedly".to_string())
    })?;
    supervisor.finish();
    result
}

struct NpmProcessSupervisor {
    cancel: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NpmProcessSupervisor {
    fn finish(&mut self) {
        self.cancel.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for NpmProcessSupervisor {
    fn drop(&mut self) {
        // Dropping the async caller signals the independent supervisor and
        // synchronously joins it, so task cancellation cannot skip tree
        // termination or leave the child unreaped.
        self.finish();
    }
}

async fn run_bounded_process_inner(
    program: &str,
    args: &[String],
    cwd: NpmProcessCwd,
    limits: NpmProcessLimits,
    environment: Vec<(OsString, Option<OsString>)>,
    cancellation: Option<std::sync::mpsc::Receiver<()>>,
) -> Result<NpmProcessOutput, NpmProcessError> {
    let isolated_dir = match &cwd {
        NpmProcessCwd::Isolated => {
            Some(tempfile::tempdir().map_err(|error| NpmProcessError::Setup(error.to_string()))?)
        }
        NpmProcessCwd::Trusted(_) => None,
    };
    let cwd = match &cwd {
        NpmProcessCwd::Isolated => isolated_dir
            .as_ref()
            .map(tempfile::TempDir::path)
            .expect("isolated npm directory was just created"),
        NpmProcessCwd::Trusted(path) => {
            let metadata = std::fs::metadata(&path)
                .map_err(|error| NpmProcessError::Setup(error.to_string()))?;
            if !metadata.is_dir() {
                return Err(NpmProcessError::Setup(format!(
                    "trusted npm cwd is not a directory: {}",
                    path.display()
                )));
            }
            path.as_path()
        }
    };

    let mut child = ManagedNpmChild::spawn(program, args, cwd, &environment)?;
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| NpmProcessError::Setup("npm stdout was not piped".to_string()))?;
    let mut stderr = child
        .take_stderr()
        .ok_or_else(|| NpmProcessError::Setup("npm stderr was not piped".to_string()))?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_chunk = [0u8; 8192];
    let mut stderr_chunk = [0u8; 8192];
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut status = None;
    let deadline = tokio::time::Instant::now() + limits.timeout;
    let deadline_sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(deadline_sleep);

    loop {
        let cancelled = cancellation.as_ref().is_some_and(|receiver| {
            !matches!(
                receiver.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            )
        });
        if cancelled {
            child.terminate().await;
            return Err(NpmProcessError::Cancelled);
        }
        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    child.terminate().await;
                    return Err(NpmProcessError::Wait(error.to_string()));
                }
            };
        }
        if status.is_some() {
            if stdout_done && stderr_done {
                let status = child
                    .wait()
                    .await
                    .map_err(|error| NpmProcessError::Wait(error.to_string()))?;
                return Ok(NpmProcessOutput {
                    status,
                    stdout: stdout_bytes,
                    stderr: stderr_bytes,
                });
            }
        }

        tokio::select! {
            biased;

            _ = &mut deadline_sleep => {
                child.terminate().await;
                return Err(NpmProcessError::TimedOut);
            }
            read = stdout.read(&mut stdout_chunk), if !stdout_done => {
                let read = match read {
                    Ok(read) => read,
                    Err(error) => {
                        child.terminate().await;
                        return Err(NpmProcessError::Read(error.to_string()));
                    }
                };
                if read == 0 {
                    stdout_done = true;
                } else if !append_bounded(
                    &mut stdout_bytes,
                    &stdout_chunk[..read],
                    stderr_bytes.len(),
                    limits.max_output_bytes,
                ) {
                    child.terminate().await;
                    return Err(NpmProcessError::OutputLimitExceeded {
                        limit: limits.max_output_bytes,
                    });
                }
            }
            read = stderr.read(&mut stderr_chunk), if !stderr_done => {
                let read = match read {
                    Ok(read) => read,
                    Err(error) => {
                        child.terminate().await;
                        return Err(NpmProcessError::Read(error.to_string()));
                    }
                };
                if read == 0 {
                    stderr_done = true;
                } else if !append_bounded(
                    &mut stderr_bytes,
                    &stderr_chunk[..read],
                    stdout_bytes.len(),
                    limits.max_output_bytes,
                ) {
                    child.terminate().await;
                    return Err(NpmProcessError::OutputLimitExceeded {
                        limit: limits.max_output_bytes,
                    });
                }
            }
            _ = tokio::time::sleep(NPM_PROCESS_POLL_INTERVAL) => {}
        }
    }
}

fn append_bounded(
    destination: &mut Vec<u8>,
    bytes: &[u8],
    other_stream_len: usize,
    max_output_bytes: usize,
) -> bool {
    let remaining =
        max_output_bytes.saturating_sub(destination.len().saturating_add(other_stream_len));
    let accepted = remaining.min(bytes.len());
    destination.extend_from_slice(&bytes[..accepted]);
    accepted == bytes.len()
}

#[cfg(unix)]
struct ManagedNpmChild {
    child: tokio::process::Child,
}

#[cfg(unix)]
impl ManagedNpmChild {
    fn spawn(
        program: &str,
        args: &[String],
        cwd: &Path,
        environment: &[(OsString, Option<OsString>)],
    ) -> Result<Self, NpmProcessError> {
        use std::os::unix::process::CommandExt;

        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(cwd);
        for (name, value) in environment {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
        command.as_std_mut().process_group(0);
        command
            .spawn()
            .map(|child| Self { child })
            .map_err(|error| NpmProcessError::Spawn(error.to_string()))
    }

    fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        self.child.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        self.child.stderr.take()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait().await
    }

    async fn terminate(&mut self) {
        if let Some(pid) = self.child.id().and_then(|pid| i32::try_from(pid).ok()) {
            unsafe {
                unix_kill(-pid, 9);
            }
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(NPM_TERMINATION_TIMEOUT, self.child.wait()).await;
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn unix_kill(pid: i32, signal: i32) -> i32;
}

#[cfg(windows)]
use windows_process::ManagedNpmChild;

#[cfg(windows)]
mod windows_process {
    use std::ffi::{OsStr, OsString};
    use std::io;
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::ExitStatusExt;
    use std::path::{Component, Path, PathBuf};
    use std::process::ExitStatus;
    use std::ptr::{null, null_mut};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, TRUE, WAIT_FAILED, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::SystemInformation::{
        GetSystemDirectoryW, GetWindowsDirectoryW,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
        InitializeProcThreadAttributeList, ResumeThread, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
        CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES,
        STARTUPINFOEXW,
    };

    use super::{NpmProcessError, NPM_PROCESS_POLL_INTERVAL, NPM_TERMINATION_TIMEOUT};

    pub(super) struct ManagedNpmChild {
        process: OwnedHandle,
        job: Option<OwnedHandle>,
        stdout: Option<tokio::fs::File>,
        stderr: Option<tokio::fs::File>,
    }

    impl ManagedNpmChild {
        pub(super) fn spawn(
            program: &str,
            args: &[String],
            cwd: &Path,
            environment: &[(OsString, Option<OsString>)],
        ) -> Result<Self, NpmProcessError> {
            Self::spawn_inner(program, args, cwd, environment)
                .map_err(|error| NpmProcessError::Spawn(error.to_string()))
        }

        fn spawn_inner(
            program: &str,
            args: &[String],
            cwd: &Path,
            environment: &[(OsString, Option<OsString>)],
        ) -> io::Result<Self> {
            let resolved_program = resolve_executable(program)?;
            let is_batch = resolved_program
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
                });
            let (application, mut command_line) = if is_batch {
                (
                    system_directory()?.join("cmd.exe"),
                    make_batch_command_line(&resolved_program, args)?,
                )
            } else {
                (resolved_program, make_command_line(program, args)?)
            };
            command_line.push(0);
            let application = encode_path_nul(&application)?;
            let cwd = encode_path_nul(cwd)?;
            let mut environment = make_environment_block(environment)?;

            let job = create_kill_on_close_job()?;
            let stdin = create_eof_stdin()?;
            let (stdout, child_stdout) = create_output_pipe()?;
            let (stderr, child_stderr) = create_output_pipe()?;
            let inherited_handles = [
                raw_handle(&stdin),
                raw_handle(&child_stdout),
                raw_handle(&child_stderr),
            ];
            let mut attributes = ProcThreadAttributeList::new(1)?;
            attributes.set_handle_list(&inherited_handles)?;

            let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
            startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = inherited_handles[0];
            startup.StartupInfo.hStdOutput = inherited_handles[1];
            startup.StartupInfo.hStdError = inherited_handles[2];
            startup.lpAttributeList = attributes.as_mut_ptr();

            let mut process_info: PROCESS_INFORMATION = unsafe { zeroed() };
            let flags = CREATE_NO_WINDOW
                | CREATE_SUSPENDED
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT;
            let created = unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    command_line.as_mut_ptr(),
                    null(),
                    null(),
                    TRUE,
                    flags,
                    environment.as_mut_ptr().cast(),
                    cwd.as_ptr(),
                    (&startup as *const STARTUPINFOEXW).cast(),
                    &mut process_info,
                )
            };
            if created == 0 {
                return Err(io::Error::last_os_error());
            }

            let process = unsafe { OwnedHandle::from_raw_handle(process_info.hProcess.cast()) };
            let thread = unsafe { OwnedHandle::from_raw_handle(process_info.hThread.cast()) };
            let assigned =
                unsafe { AssignProcessToJobObject(raw_handle(&job), raw_handle(&process)) };
            if assigned == 0 {
                let error = io::Error::last_os_error();
                terminate_suspended_process(&process, None);
                return Err(io::Error::new(
                    error.kind(),
                    format!("could not assign npm process to its Windows Job Object: {error}"),
                ));
            }

            let resumed = unsafe { ResumeThread(raw_handle(&thread)) };
            if resumed == u32::MAX {
                let error = io::Error::last_os_error();
                terminate_suspended_process(&process, Some(&job));
                return Err(io::Error::new(
                    error.kind(),
                    format!("could not resume job-bound npm process: {error}"),
                ));
            }
            drop(thread);
            drop(stdin);
            drop(child_stdout);
            drop(child_stderr);

            let stdout = tokio::fs::File::from_std(std::fs::File::from(stdout));
            let stderr = tokio::fs::File::from_std(std::fs::File::from(stderr));
            Ok(Self {
                process,
                job: Some(job),
                stdout: Some(stdout),
                stderr: Some(stderr),
            })
        }

        pub(super) fn take_stdout(&mut self) -> Option<tokio::fs::File> {
            self.stdout.take()
        }

        pub(super) fn take_stderr(&mut self) -> Option<tokio::fs::File> {
            self.stderr.take()
        }

        pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            match unsafe { WaitForSingleObject(raw_handle(&self.process), 0) } {
                WAIT_TIMEOUT => Ok(None),
                WAIT_OBJECT_0 => {
                    let mut exit_code = 0;
                    if unsafe { GetExitCodeProcess(raw_handle(&self.process), &mut exit_code) } == 0
                    {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(Some(ExitStatus::from_raw(exit_code)))
                    }
                }
                WAIT_FAILED => Err(io::Error::last_os_error()),
                result => Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("unexpected Windows process wait result: {result}"),
                )),
            }
        }

        pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
            loop {
                if let Some(status) = self.try_wait()? {
                    return Ok(status);
                }
                tokio::time::sleep(NPM_PROCESS_POLL_INTERVAL).await;
            }
        }

        pub(super) async fn terminate(&mut self) {
            self.terminate_job();
            let _ = tokio::time::timeout(NPM_TERMINATION_TIMEOUT, self.wait()).await;
        }

        fn terminate_job(&mut self) {
            if let Some(job) = self.job.take() {
                unsafe {
                    TerminateJobObject(raw_handle(&job), 1);
                    // A job is signalled only after its active process count
                    // reaches zero. Keep the handle open while waiting so
                    // cancellation does not merely enqueue termination and
                    // return while descendants are still alive.
                    WaitForSingleObject(raw_handle(&job), duration_millis(NPM_TERMINATION_TIMEOUT));
                }
                // KILL_ON_JOB_CLOSE is the fail-safe if explicit termination
                // races with process teardown or returns an error.
                drop(job);
            }
            unsafe {
                TerminateProcess(raw_handle(&self.process), 1);
            }
        }
    }

    impl Drop for ManagedNpmChild {
        fn drop(&mut self) {
            self.terminate_job();
        }
    }

    struct ProcThreadAttributeList {
        storage: Vec<usize>,
        initialized: bool,
    }

    impl ProcThreadAttributeList {
        fn new(attribute_count: u32) -> io::Result<Self> {
            let mut required_bytes = 0usize;
            unsafe {
                InitializeProcThreadAttributeList(
                    null_mut(),
                    attribute_count,
                    0,
                    &mut required_bytes,
                );
            }
            if required_bytes == 0 {
                return Err(io::Error::last_os_error());
            }
            let words = required_bytes
                .checked_add(size_of::<usize>() - 1)
                .and_then(|bytes| bytes.checked_div(size_of::<usize>()))
                .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "attribute list too large"))?;
            let mut list = Self {
                storage: vec![0usize; words],
                initialized: false,
            };
            if unsafe {
                InitializeProcThreadAttributeList(
                    list.as_mut_ptr(),
                    attribute_count,
                    0,
                    &mut required_bytes,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            list.initialized = true;
            Ok(list)
        }

        fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
            self.storage.as_mut_ptr().cast()
        }

        fn set_handle_list(&mut self, handles: &[HANDLE]) -> io::Result<()> {
            if unsafe {
                UpdateProcThreadAttribute(
                    self.as_mut_ptr(),
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    handles.as_ptr().cast(),
                    std::mem::size_of_val(handles),
                    null_mut(),
                    null(),
                )
            } == 0
            {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    impl Drop for ProcThreadAttributeList {
        fn drop(&mut self) {
            if self.initialized {
                unsafe {
                    DeleteProcThreadAttributeList(self.as_mut_ptr());
                }
            }
        }
    }

    fn create_kill_on_close_job() -> io::Result<OwnedHandle> {
        let raw_job = unsafe { CreateJobObjectW(null(), null()) };
        let job = owned_handle(raw_job)?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        if unsafe {
            SetInformationJobObject(
                raw_handle(&job),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn create_output_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let (read, write) = create_inheritable_pipe()?;
        if unsafe {
            windows_sys::Win32::Foundation::SetHandleInformation(
                raw_handle(&read),
                HANDLE_FLAG_INHERIT,
                0,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((read, write))
    }

    fn create_eof_stdin() -> io::Result<OwnedHandle> {
        let (read, write) = create_inheritable_pipe()?;
        drop(write);
        Ok(read)
    }

    fn create_inheritable_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
            lpSecurityDescriptor: null_mut(),
            bInheritHandle: TRUE,
        };
        let mut read = null_mut();
        let mut write = null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, &attributes, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((owned_handle(read)?, owned_handle(write)?))
    }

    fn owned_handle(handle: HANDLE) -> io::Result<OwnedHandle> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
        }
    }

    fn raw_handle(handle: &OwnedHandle) -> HANDLE {
        handle.as_raw_handle().cast()
    }

    fn terminate_suspended_process(process: &OwnedHandle, job: Option<&OwnedHandle>) {
        if let Some(job) = job {
            unsafe {
                TerminateJobObject(raw_handle(job), 1);
            }
        }
        unsafe {
            TerminateProcess(raw_handle(process), 1);
            WaitForSingleObject(
                raw_handle(process),
                duration_millis(NPM_TERMINATION_TIMEOUT),
            );
        }
    }

    fn duration_millis(duration: Duration) -> u32 {
        u32::try_from(duration.as_millis()).unwrap_or(u32::MAX - 1)
    }

    fn resolve_executable(program: &str) -> io::Result<PathBuf> {
        ensure_no_nul(program)?;
        let path = Path::new(program);
        if program.is_empty() || path.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "program path has no file name",
            ));
        }
        let is_file_name = matches!(
            path.components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        );
        if !is_file_name {
            if has_ascii_extension(path, "exe") {
                return Ok(path.to_path_buf());
            }
            let executable = append_suffix(path, ".exe");
            return Ok(if program_exists(&executable) {
                executable
            } else {
                path.to_path_buf()
            });
        }

        let has_extension = program.as_bytes().contains(&b'.');
        let file_name = if has_extension {
            OsString::from(program)
        } else {
            OsString::from(format!("{program}.exe"))
        };
        let mut directories = Vec::new();
        if let Ok(mut current_exe) = std::env::current_exe() {
            current_exe.pop();
            directories.push(current_exe);
        }
        directories.push(system_directory()?);
        directories.push(windows_directory()?);
        if let Some(path) = std::env::var_os("PATH") {
            directories
                .extend(std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()));
        }
        directories
            .into_iter()
            .map(|directory| directory.join(&file_name))
            .find(|candidate| program_exists(candidate))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "program not found"))
    }

    fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        PathBuf::from(value)
    }

    fn has_ascii_extension(path: &Path, expected: &str) -> bool {
        path.extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
    }

    fn program_exists(path: &Path) -> bool {
        std::fs::metadata(path)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
    }

    fn make_command_line(program: &str, args: &[String]) -> io::Result<Vec<u16>> {
        ensure_no_nul(program)?;
        if program.contains('"') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "program paths may not contain quotes",
            ));
        }
        let mut command_line = vec![b'"' as u16];
        command_line.extend(program.encode_utf16());
        command_line.push(b'"' as u16);
        for arg in args {
            command_line.push(b' ' as u16);
            append_command_arg(&mut command_line, arg)?;
        }
        Ok(command_line)
    }

    fn append_command_arg(command_line: &mut Vec<u16>, arg: &str) -> io::Result<()> {
        ensure_no_nul(arg)?;
        let quote = arg.is_empty()
            || arg
                .as_bytes()
                .iter()
                .any(|byte| matches!(byte, b' ' | b'\t'));
        if quote {
            command_line.push(b'"' as u16);
        }
        let mut backslashes = 0usize;
        for unit in arg.encode_utf16() {
            if unit == b'\\' as u16 {
                backslashes += 1;
            } else {
                if unit == b'"' as u16 {
                    command_line.extend((0..=backslashes).map(|_| b'\\' as u16));
                }
                backslashes = 0;
            }
            command_line.push(unit);
        }
        if quote {
            command_line.extend((0..backslashes).map(|_| b'\\' as u16));
            command_line.push(b'"' as u16);
        }
        Ok(())
    }

    fn make_batch_command_line(script: &Path, args: &[String]) -> io::Result<Vec<u16>> {
        // This is the hardened encoding used by Rust 1.78's
        // `std::process::Command` after CVE-2024-24576. Batch shims inherently
        // require cmd.exe, so every argument must use cmd-aware escaping.
        let script = encode_path(script)?;
        if script.contains(&(b'"' as u16)) || script.last() == Some(&(b'\\' as u16)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows file names may not contain quotes or end with a backslash",
            ));
        }
        let mut command_line: Vec<u16> = "cmd.exe /e:ON /v:OFF /d /c \"".encode_utf16().collect();
        command_line.push(b'"' as u16);
        command_line.extend(script);
        command_line.push(b'"' as u16);
        for arg in args {
            if arg.contains(['\r', '\n', '"']) || arg.ends_with('\\') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "batch file argument cannot be represented safely and exactly",
                ));
            }
            command_line.push(b' ' as u16);
            append_batch_arg(&mut command_line, arg)?;
        }
        command_line.push(b'"' as u16);
        Ok(command_line)
    }

    fn append_batch_arg(command_line: &mut Vec<u16>, arg: &str) -> io::Result<()> {
        ensure_no_nul(arg)?;
        const UNQUOTED: &str = r"#$*+-./:?@\_";
        let mut quote = arg.is_empty() || arg.as_bytes().last() == Some(&b'\\');
        quote |= arg.chars().any(|character| {
            (character.is_ascii()
                && !(character.is_ascii_alphanumeric() || UNQUOTED.contains(character)))
                || character.is_control()
        });
        if quote {
            command_line.push(b'"' as u16);
        }
        let mut backslashes = 0usize;
        for unit in arg.encode_utf16() {
            if unit == b'\\' as u16 {
                backslashes += 1;
            } else {
                if unit == b'"' as u16 {
                    command_line.extend((0..backslashes).map(|_| b'\\' as u16));
                    command_line.push(b'"' as u16);
                } else if unit == b'%' as u16 {
                    command_line.extend("%%cd:~,".encode_utf16());
                }
                backslashes = 0;
            }
            command_line.push(unit);
        }
        if quote {
            command_line.extend((0..backslashes).map(|_| b'\\' as u16));
            command_line.push(b'"' as u16);
        }
        Ok(())
    }

    fn ensure_no_nul(value: &str) -> io::Result<()> {
        if value.contains('\0') {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nul byte found in provided data",
            ))
        } else {
            Ok(())
        }
    }

    fn make_environment_block(changes: &[(OsString, Option<OsString>)]) -> io::Result<Vec<u16>> {
        let mut entries = std::env::vars_os().collect::<Vec<_>>();
        for (name, value) in changes {
            validate_environment_change_name(name)?;
            entries.retain(|(existing, _)| !environment_names_equal(existing, name));
            if let Some(value) = value {
                ensure_os_string_has_no_nul(value, "environment variable value")?;
                entries.push((name.clone(), value.clone()));
            }
        }
        entries.sort_by_key(|(name, _)| name.to_string_lossy().to_uppercase());

        let mut block = Vec::new();
        for (name, value) in entries {
            append_environment_entry(&mut block, &name, &value)?;
        }
        // CreateProcessW requires a double-NUL-terminated Unicode block,
        // including when the inherited environment happens to be empty.
        block.push(0);
        if block.len() == 1 {
            block.push(0);
        }
        Ok(block)
    }

    fn validate_environment_change_name(name: &OsStr) -> io::Result<()> {
        ensure_os_string_has_no_nul(name, "environment variable name")?;
        let encoded = name.encode_wide().collect::<Vec<_>>();
        if encoded.is_empty() || encoded.contains(&(b'=' as u16)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "environment variable name must be non-empty and contain no equals sign",
            ));
        }
        Ok(())
    }

    fn append_environment_entry(
        block: &mut Vec<u16>,
        name: &OsStr,
        value: &OsStr,
    ) -> io::Result<()> {
        ensure_os_string_has_no_nul(name, "environment variable name")?;
        ensure_os_string_has_no_nul(value, "environment variable value")?;
        let encoded_name = name.encode_wide().collect::<Vec<_>>();
        if encoded_name.is_empty()
            || encoded_name
                .iter()
                .skip(usize::from(encoded_name.first() == Some(&(b'=' as u16))))
                .any(|unit| *unit == b'=' as u16)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inherited environment contained an invalid variable name",
            ));
        }
        block.extend(encoded_name);
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
        Ok(())
    }

    fn ensure_os_string_has_no_nul(value: &OsStr, kind: &str) -> io::Result<()> {
        if value.encode_wide().any(|unit| unit == 0) {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("nul byte found in {kind}"),
            ))
        } else {
            Ok(())
        }
    }

    fn environment_names_equal(left: &OsStr, right: &OsStr) -> bool {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }

    fn encode_path_nul(path: &Path) -> io::Result<Vec<u16>> {
        let mut encoded = encode_path(path)?;
        encoded.push(0);
        Ok(encoded)
    }

    fn encode_path(path: &Path) -> io::Result<Vec<u16>> {
        let mut encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if encoded.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nul byte found in provided path",
            ));
        }
        const VERBATIM: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        const UNC: &[u16] = &[b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];
        if encoded.starts_with(VERBATIM) {
            encoded.drain(..VERBATIM.len());
            if encoded.starts_with(UNC) {
                encoded.drain(..UNC.len());
                encoded.splice(0..0, [b'\\' as u16, b'\\' as u16]);
            }
        }
        Ok(encoded)
    }

    fn system_directory() -> io::Result<PathBuf> {
        windows_directory_from(GetSystemDirectoryW)
    }

    fn windows_directory() -> io::Result<PathBuf> {
        windows_directory_from(GetWindowsDirectoryW)
    }

    fn windows_directory_from(
        api: unsafe extern "system" fn(*mut u16, u32) -> u32,
    ) -> io::Result<PathBuf> {
        let mut buffer = vec![0u16; 260];
        loop {
            let length = unsafe {
                api(
                    buffer.as_mut_ptr(),
                    u32::try_from(buffer.len()).unwrap_or(u32::MAX),
                )
            };
            if length == 0 {
                return Err(io::Error::last_os_error());
            }
            let length = usize::try_from(length)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if length < buffer.len() {
                buffer.truncate(length);
                return Ok(PathBuf::from(OsString::from_wide(&buffer)));
            }
            buffer.resize(length.saturating_add(1), 0);
        }
    }
}

fn parse_single_absolute_line(output: &str) -> Option<PathBuf> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let trimmed = lines.next()?.trim();
    if lines.next().is_some()
        || trimmed.is_empty()
        || trimmed.chars().any(|character| character.is_control())
    {
        return None;
    }
    let path = PathBuf::from(trimmed);
    path.is_absolute().then_some(path)
}

fn validate_global_root(root: &Path) -> Option<PathBuf> {
    if !root.is_absolute() {
        return None;
    }
    let canonical = std::fs::canonicalize(root).ok()?;
    if !canonical.is_absolute()
        || !canonical.is_dir()
        || canonical.file_name().and_then(|name| name.to_str()) != Some("node_modules")
    {
        return None;
    }
    Some(canonical)
}

/// Return a canonical package directory only when it is exactly one npm
/// package below a canonical global `node_modules` root. In particular, a
/// package symlink resolving outside that root is rejected.
fn validate_global_package_path(root: &Path, package_name: &str) -> Option<PathBuf> {
    let canonical_root = validate_global_root(root)?;
    let relative = package_relative_path(package_name)?;
    let lexical = canonical_root.join(&relative);
    if !lexical.is_dir() || !is_regular_file(&lexical.join("package.json")) {
        return None;
    }
    let canonical_package = std::fs::canonicalize(&lexical).ok()?;
    if !canonical_package.is_dir() || !is_regular_file(&canonical_package.join("package.json")) {
        return None;
    }
    let actual_relative = canonical_package.strip_prefix(&canonical_root).ok()?;
    same_components(actual_relative, &relative).then_some(canonical_package)
}

#[cfg(test)]
fn parse_pnpm_global_package_path(output: &str, package_name: &str) -> Option<PathBuf> {
    let package_names = [package_name.to_string()];
    parse_pnpm_global_package_paths(output, &package_names).remove(package_name)
}

fn parse_pnpm_global_package_paths(
    output: &str,
    package_names: &[String],
) -> std::collections::HashMap<String, PathBuf> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
        return std::collections::HashMap::new();
    };
    let entries = value
        .as_array()
        .map(|entries| entries.as_slice())
        .unwrap_or_else(|| std::slice::from_ref(&value));
    let mut packages = std::collections::HashMap::new();
    for entry in entries {
        let Some(root_hint) = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .and_then(|path| validate_absolute_directory(Path::new(path)))
        else {
            continue;
        };
        let Some(dependencies) = entry
            .get("dependencies")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for package_name in package_names {
            if packages.contains_key(package_name) {
                continue;
            }
            let Some(path) = dependencies
                .get(package_name)
                .and_then(|dependency| dependency.get("path"))
                .and_then(serde_json::Value::as_str)
                .map(Path::new)
                .and_then(|path| validate_pnpm_package_path(path, package_name, &root_hint))
            else {
                continue;
            };
            packages.insert(package_name.clone(), path);
        }
    }
    packages
}

fn validate_pnpm_package_path(
    path: &Path,
    package_name: &str,
    root_hint: &Path,
) -> Option<PathBuf> {
    if !path.is_absolute() || !path.is_dir() || !is_regular_file(&path.join("package.json")) {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    if !canonical.is_dir() || !is_regular_file(&canonical.join("package.json")) {
        return None;
    }
    if canonical == root_hint || !canonical.starts_with(root_hint) {
        return None;
    }
    let (_node_modules, relative) = nearest_node_modules(&canonical)?;
    let expected = package_relative_path(package_name)?;
    same_components(relative, &expected).then_some(canonical)
}

fn validate_absolute_directory(path: &Path) -> Option<PathBuf> {
    (path.is_absolute() && path.is_dir())
        .then(|| std::fs::canonicalize(path).ok())
        .flatten()
        .filter(|path| path.is_absolute() && path.is_dir())
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        .unwrap_or(false)
}

fn nearest_node_modules(path: &Path) -> Option<(&Path, &Path)> {
    let mut current = path;
    while let Some(parent) = current.parent() {
        if current.file_name().and_then(|name| name.to_str()) == Some("node_modules") {
            return Some((current, path.strip_prefix(current).ok()?));
        }
        current = parent;
    }
    None
}

fn package_relative_path(package_name: &str) -> Option<PathBuf> {
    if !valid_package_name(package_name) {
        return None;
    }
    let path = PathBuf::from(package_name);
    let components = path.components().collect::<Vec<_>>();
    match components.as_slice() {
        [Component::Normal(scope), Component::Normal(name)]
            if package_name.starts_with('@')
                && scope.to_string_lossy().starts_with('@')
                && !name.is_empty() =>
        {
            Some(path)
        }
        [Component::Normal(name)] if !package_name.starts_with('@') && !name.is_empty() => {
            Some(path)
        }
        _ => None,
    }
}

fn valid_package_name(package_name: &str) -> bool {
    if package_name.is_empty()
        || package_name.chars().any(|character| character.is_control())
        || package_name.contains('\\')
    {
        return false;
    }
    if let Some(rest) = package_name.strip_prefix('@') {
        let Some((scope, name)) = rest.split_once('/') else {
            return false;
        };
        !scope.is_empty()
            && !name.is_empty()
            && !name.contains('/')
            && scope != "."
            && scope != ".."
            && name != "."
            && name != ".."
    } else {
        !package_name.contains('/') && package_name != "." && package_name != ".."
    }
}

fn same_components(actual: &Path, expected: &Path) -> bool {
    let actual = actual.components().collect::<Vec<_>>();
    let expected = expected.components().collect::<Vec<_>>();
    actual == expected
}

fn truncate_output(value: &str) -> String {
    const MAX: usize = 512;
    if value.len() <= MAX {
        return value.to_string();
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= MAX)
        .last()
        .unwrap_or(0);
    format!("{}...", &value[..end])
}

fn select_argv<'a>(
    project_settings: &'a [Settings],
    global_settings: &'a Settings,
    project_trusted: bool,
) -> Option<&'a [String]> {
    if project_trusted {
        // `load_project_settings` returns `.rpi` before `.pi`. An explicitly
        // empty command is still a value: it selects default npm and masks the
        // lower-precedence settings, matching Pi's merged settings behavior.
        for settings in project_settings {
            if let Some(command) = settings.npm_command.as_deref() {
                return Some(command);
            }
        }
    }
    global_settings.npm_command.as_deref()
}

fn manager_kind(program: &str, prefix_args: &[String]) -> NpmManagerKind {
    let mut command_parts = Vec::with_capacity(prefix_args.len() + 1);
    command_parts.push(program);
    command_parts.extend(prefix_args.iter().map(String::as_str));
    let manager_command = match command_parts.iter().rposition(|part| *part == "--") {
        Some(index) => command_parts.get(index + 1).copied().unwrap_or(""),
        None => program,
    };
    let file_name = manager_command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(manager_command);
    let normalized = file_name.to_ascii_lowercase();
    let normalized = normalized
        .strip_suffix(".cmd")
        .or_else(|| normalized.strip_suffix(".exe"))
        .unwrap_or(&normalized);
    match normalized {
        "npm" => NpmManagerKind::Npm,
        "pnpm" => NpmManagerKind::Pnpm,
        "bun" => NpmManagerKind::Bun,
        _ => NpmManagerKind::Other,
    }
}

fn default_npm_program() -> &'static str {
    if cfg!(windows) {
        "npm.cmd"
    } else {
        "npm"
    }
}

fn windows_command_program(program: &str) -> String {
    if cfg!(windows)
        && !program.contains(['/', '\\'])
        && matches!(
            program.to_ascii_lowercase().as_str(),
            "npm" | "pnpm" | "yarn" | "npx" | "corepack"
        )
    {
        format!("{program}.cmd")
    } else {
        program.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use super::{
        parse_pnpm_global_package_path, parse_pnpm_global_package_paths,
        parse_single_absolute_line, run_bounded_command, run_bounded_process, select_argv,
        validate_global_package_path, NpmCommand, NpmManagerKind, NpmProcessCwd, NpmProcessError,
        NpmProcessLimits,
    };
    use crate::settings::Settings;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[cfg(windows)]
    fn powershell_program() -> String {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe")
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(windows)]
    fn script_command(script: String) -> (String, Vec<String>) {
        (
            powershell_program(),
            vec![
                "-NoLogo".to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                script,
            ],
        )
    }

    #[cfg(unix)]
    fn script_command(script: String) -> (String, Vec<String>) {
        ("/bin/sh".to_string(), vec!["-c".to_string(), script])
    }

    fn test_limits(timeout: Duration, max_output_bytes: usize) -> NpmProcessLimits {
        NpmProcessLimits {
            timeout,
            max_output_bytes,
        }
    }

    #[cfg(windows)]
    fn descendant_script(launched: &Path, survivor: &Path) -> String {
        use base64::Engine;

        // The wrapper deliberately exits immediately. `-NoNewWindow` makes
        // the descendant inherit its stdout/stderr pipe handles, reproducing
        // the case where a PID-based tree walk can no longer find it.
        let survivor = survivor.to_string_lossy().replace('\'', "''");
        let inner =
            format!("Start-Sleep -Seconds 4; [IO.File]::WriteAllText('{survivor}', 'alive')");
        let encoded_bytes = inner
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let encoded = base64::engine::general_purpose::STANDARD.encode(encoded_bytes);
        let powershell = powershell_program().replace('\'', "''");
        let launched = launched.to_string_lossy().replace('\'', "''");
        format!(
            "$null = Start-Process -NoNewWindow -FilePath '{powershell}' \
             -ArgumentList @('-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand','{encoded}'); \
             [IO.File]::WriteAllText('{launched}', 'launched')"
        )
    }

    #[cfg(unix)]
    fn descendant_script(launched: &Path, survivor: &Path) -> String {
        let launched = launched.to_string_lossy().replace('\'', "'\"'\"'");
        let survivor = survivor.to_string_lossy().replace('\'', "'\"'\"'");
        format!("(sleep 3; printf alive > '{survivor}') & printf launched > '{launched}'; sleep 30")
    }

    #[test]
    fn configured_argv_is_never_shell_parsed() {
        let argv = strings(&["mise", "exec", "node@20", "--", "npm"]);
        let command = NpmCommand::from_argv(Some(&argv)).unwrap();
        assert_eq!(command.program(), "mise");
        assert_eq!(
            command.combined_args(&strings(&["view", "demo", "version", "--json"])),
            strings(&["exec", "node@20", "--", "npm", "view", "demo", "version", "--json"])
        );
        assert_eq!(command.manager_kind(), NpmManagerKind::Npm);
        assert!(command.is_configured());
    }

    #[tokio::test]
    async fn bounded_runner_isolates_untrusted_cwd_and_honors_trusted_cwd() {
        #[cfg(windows)]
        let script = "[Console]::Out.Write((Get-Location).Path)".to_string();
        #[cfg(unix)]
        let script = "printf %s \"$PWD\"".to_string();
        let (program, args) = script_command(script.clone());
        let output = run_bounded_process(
            &program,
            &args,
            NpmProcessCwd::Isolated,
            test_limits(Duration::from_secs(5), 4096),
        )
        .await
        .unwrap();
        assert!(output.status.success());
        let isolated_cwd = PathBuf::from(String::from_utf8(output.stdout).unwrap());
        assert!(isolated_cwd.is_absolute());
        assert_ne!(isolated_cwd, std::env::current_dir().unwrap());
        assert!(
            !isolated_cwd.exists(),
            "isolated cwd should be removed after the npm lookup"
        );

        let trusted = tempfile::tempdir().unwrap();
        let (program, args) = script_command(script);
        let output = run_bounded_process(
            &program,
            &args,
            NpmProcessCwd::Trusted(trusted.path().to_path_buf()),
            test_limits(Duration::from_secs(5), 4096),
        )
        .await
        .unwrap();
        assert!(output.status.success());
        let reported = PathBuf::from(String::from_utf8(output.stdout).unwrap());
        assert_eq!(
            std::fs::canonicalize(reported).unwrap(),
            std::fs::canonicalize(trusted.path()).unwrap()
        );
    }

    #[tokio::test]
    async fn bounded_command_applies_environment_overrides() {
        let _guard = crate::config::test_support::env_lock().lock().unwrap();
        let set_name = format!("RPI_BOUNDED_SET_{}", std::process::id());
        let remove_name = format!("RPI_BOUNDED_REMOVE_{}", std::process::id());
        let previous_set = std::env::var_os(&set_name);
        let previous_remove = std::env::var_os(&remove_name);
        std::env::set_var(&set_name, "parent");
        std::env::set_var(&remove_name, "parent");

        #[cfg(windows)]
        let script = format!(
            "$removed = [Environment]::GetEnvironmentVariable('{remove_name}', 'Process'); \
             if ($null -eq $removed) {{ $removed = '<missing>' }}; \
             [Console]::Out.Write([Environment]::GetEnvironmentVariable('{set_name}', 'Process') + '|' + $removed)"
        );
        #[cfg(unix)]
        let script = format!(
            "printf '%s|%s' \"${{{set_name}-<missing>}}\" \"${{{remove_name}-<missing>}}\""
        );
        let (program, args) = script_command(script);
        let cwd = tempfile::tempdir().unwrap();
        let environment = vec![
            (OsString::from(&set_name), Some(OsString::from("child"))),
            (OsString::from(&remove_name), None),
        ];

        let result = run_bounded_command(
            &program,
            &args,
            cwd.path(),
            &environment,
            Duration::from_secs(5),
            4096,
        )
        .await;

        match previous_set {
            Some(value) => std::env::set_var(&set_name, value),
            None => std::env::remove_var(&set_name),
        }
        match previous_remove {
            Some(value) => std::env::set_var(&remove_name, value),
            None => std::env::remove_var(&remove_name),
        }
        let output = result.unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "child|<missing>");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn startup_remediation_blocking_runner_is_safe_inside_tokio() {
        #[cfg(windows)]
        let script = "[Console]::Out.Write((Get-Location).Path)".to_string();
        #[cfg(unix)]
        let script = "printf %s \"$PWD\"".to_string();
        let (program, prefix_args) = script_command(script);
        let mut argv = vec![program];
        argv.extend(prefix_args);
        let command = NpmCommand::from_argv(Some(&argv)).unwrap();
        let trusted = tempfile::tempdir().unwrap();

        let output = command
            .run_startup_remediation_with_limits(
                &[],
                trusted.path(),
                test_limits(Duration::from_secs(5), 4096),
            )
            .unwrap();

        assert!(output.status.success());
        let reported = PathBuf::from(String::from_utf8(output.stdout).unwrap());
        assert_eq!(
            std::fs::canonicalize(reported).unwrap(),
            std::fs::canonicalize(trusted.path()).unwrap()
        );
    }

    #[test]
    fn startup_remediation_blocking_runner_enforces_timeout() {
        #[cfg(windows)]
        let script = "Start-Sleep -Seconds 30".to_string();
        #[cfg(unix)]
        let script = "sleep 30".to_string();
        let (program, prefix_args) = script_command(script);
        let mut argv = vec![program];
        argv.extend(prefix_args);
        let command = NpmCommand::from_argv(Some(&argv)).unwrap();
        let trusted = tempfile::tempdir().unwrap();
        let started = Instant::now();

        let error = command
            .run_startup_remediation_with_limits(
                &[],
                trusted.path(),
                test_limits(Duration::from_secs(1), 4096),
            )
            .unwrap_err();

        assert_eq!(error, NpmProcessError::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "blocking startup timeout did not terminate promptly: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn output_limit_error_reports_the_effective_budget() {
        assert_eq!(
            NpmProcessError::OutputLimitExceeded { limit: 1024 * 1024 }.to_string(),
            "npm process exceeded the 1048576 byte output limit"
        );
    }

    #[tokio::test]
    async fn bounded_runner_enforces_combined_stdout_stderr_limit() {
        #[cfg(windows)]
        let script = concat!(
            "[Console]::Out.Write(('o' * 700)); ",
            "[Console]::Error.Write(('e' * 700)); ",
            "Start-Sleep -Seconds 30"
        )
        .to_string();
        #[cfg(unix)]
        let script = concat!(
            "i=0; while [ $i -lt 700 ]; do printf o; i=$((i+1)); done; ",
            "i=0; while [ $i -lt 700 ]; do printf e >&2; i=$((i+1)); done; ",
            "sleep 30"
        )
        .to_string();
        let (program, args) = script_command(script);
        let started = Instant::now();

        let error = run_bounded_process(
            &program,
            &args,
            NpmProcessCwd::Isolated,
            test_limits(Duration::from_secs(10), 1024),
        )
        .await
        .unwrap_err();

        assert_eq!(error, NpmProcessError::OutputLimitExceeded { limit: 1024 });
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "output overflow did not terminate promptly: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn bounded_runner_timeout_terminates_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let launched = temp.path().join("child-launched");
        let survivor = temp.path().join("child-survived");
        let script = descendant_script(&launched, &survivor);
        let (program, args) = script_command(script);
        let started = Instant::now();
        #[cfg(windows)]
        let timeout = Duration::from_secs(3);
        #[cfg(unix)]
        let timeout = Duration::from_millis(1500);

        let error = run_bounded_process(
            &program,
            &args,
            NpmProcessCwd::Isolated,
            test_limits(timeout, 4096),
        )
        .await
        .unwrap_err();

        assert_eq!(error, NpmProcessError::TimedOut);
        assert!(launched.exists(), "test descendant was not launched");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout cleanup did not finish promptly: {:?}",
            started.elapsed()
        );
        #[cfg(windows)]
        let survivor_check_delay = Duration::from_secs(5);
        #[cfg(unix)]
        let survivor_check_delay = Duration::from_secs(2);
        tokio::time::sleep(survivor_check_delay).await;
        assert!(
            !survivor.exists(),
            "npm descendant remained alive after timeout"
        );
    }

    #[tokio::test]
    async fn cancelling_runner_still_terminates_and_reaps_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let launched = temp.path().join("cancel-child-launched");
        let survivor = temp.path().join("cancel-child-survived");
        let (program, args) = script_command(descendant_script(&launched, &survivor));
        let handle = tokio::spawn(async move {
            run_bounded_process(
                &program,
                &args,
                NpmProcessCwd::Isolated,
                test_limits(Duration::from_secs(30), 4096),
            )
            .await
        });

        let launch_deadline = Instant::now() + Duration::from_secs(3);
        while !launched.exists() && Instant::now() < launch_deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(launched.exists(), "test descendant was not launched");
        let cancelled_at = Instant::now();
        handle.abort();
        assert!(handle.await.unwrap_err().is_cancelled());
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(5),
            "cancellation cleanup did not finish promptly: {:?}",
            cancelled_at.elapsed()
        );

        #[cfg(windows)]
        let survivor_check_delay = Duration::from_secs(5);
        #[cfg(unix)]
        let survivor_check_delay = Duration::from_secs(3);
        tokio::time::sleep(survivor_check_delay).await;
        assert!(
            !survivor.exists(),
            "npm descendant remained alive after caller cancellation"
        );
    }

    #[test]
    fn view_args_accept_native_and_prefixed_specs() {
        let command = NpmCommand::from_argv(Some(&strings(&["pnpm"]))).unwrap();
        assert_eq!(
            command.view_args("npm:@scope/demo@^1").unwrap(),
            strings(&["view", "@scope/demo@^1", "version", "--json"])
        );
        assert!(command.view_args("npm:   ").is_err());
        assert!(command.view_args("npm:--help").is_err());
    }

    #[test]
    fn root_install_args_match_native_pi_for_each_manager() {
        let specs = strings(&["one@latest", "@scope/two@^2"]);
        let root = Path::new("install-root");

        let npm = NpmCommand::from_argv(Some(&strings(&["npm"]))).unwrap();
        assert_eq!(
            npm.install_args(&specs, root),
            strings(&[
                "install",
                "one@latest",
                "@scope/two@^2",
                "--prefix",
                "install-root",
                "--legacy-peer-deps",
            ])
        );

        let pnpm = NpmCommand::from_argv(Some(&strings(&["pnpm"]))).unwrap();
        assert_eq!(
            pnpm.install_args(&specs, root),
            strings(&[
                "install",
                "one@latest",
                "@scope/two@^2",
                "--prefix",
                "install-root",
                "--config.auto-install-peers=false",
                "--config.strict-peer-dependencies=false",
                "--config.strict-dep-builds=false",
            ])
        );

        let bun = NpmCommand::from_argv(Some(&strings(&["bun"]))).unwrap();
        assert_eq!(
            bun.install_args(&specs, root),
            strings(&[
                "install",
                "one@latest",
                "@scope/two@^2",
                "--cwd",
                "install-root",
                "--omit=peer",
            ])
        );
    }

    #[test]
    fn root_uninstall_args_match_native_pi_for_each_manager() {
        let root = Path::new("install-root");

        let npm = NpmCommand::from_argv(Some(&strings(&["npm"]))).unwrap();
        assert_eq!(
            npm.uninstall_args("@scope/demo", root),
            strings(&[
                "uninstall",
                "@scope/demo",
                "--prefix",
                "install-root",
                "--legacy-peer-deps",
            ])
        );

        let pnpm = NpmCommand::from_argv(Some(&strings(&["pnpm"]))).unwrap();
        assert_eq!(
            pnpm.uninstall_args("demo", root),
            strings(&["uninstall", "demo", "--prefix", "install-root"])
        );

        let bun = NpmCommand::from_argv(Some(&strings(&["bun"]))).unwrap();
        assert_eq!(
            bun.uninstall_args("demo", root),
            strings(&["uninstall", "demo", "--cwd", "install-root"])
        );
    }

    #[test]
    fn blank_program_fails_closed_while_empty_array_selects_default() {
        assert!(NpmCommand::from_argv(Some(&strings(&["   ", "install"]))).is_err());
        let empty = Vec::new();
        let default = NpmCommand::from_argv(Some(&empty)).unwrap();
        assert_eq!(default.manager_kind(), NpmManagerKind::Npm);
        assert!(!default.is_configured());
    }

    #[cfg(windows)]
    #[test]
    fn windows_normalizes_bare_script_shims_but_not_wrappers() {
        let npm = NpmCommand::from_argv(Some(&strings(&["npm"]))).unwrap();
        let pnpm = NpmCommand::from_argv(Some(&strings(&["pnpm"]))).unwrap();
        let wrapper = NpmCommand::from_argv(Some(&strings(&["mise", "--", "npm"]))).unwrap();
        assert_eq!(npm.program(), "npm.cmd");
        assert_eq!(pnpm.program(), "pnpm.cmd");
        assert_eq!(wrapper.program(), "mise");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_batch_shim_round_trips_argv_without_cmd_injection() {
        let temp = tempfile::tempdir().unwrap();
        let recorder = temp.path().join("record-argv.vbs");
        let shim = temp.path().join("fake-npm.cmd");
        let launched = temp.path().join("shim-launched");
        let injected = temp.path().join("injected");
        std::fs::write(
            &recorder,
            concat!(
                "For i = 0 To WScript.Arguments.Count - 1\r\n",
                "  WScript.StdOut.Write WScript.Arguments(i)\r\n",
                "  WScript.StdOut.Write ChrW(0)\r\n",
                "Next\r\n",
            ),
        )
        .unwrap();
        std::fs::write(
            &shim,
            format!(
                "@echo off\r\nsetlocal DisableDelayedExpansion\r\n\
                 > \"{}\" echo launched\r\n\
                 cscript.exe //nologo //U \"{}\" %*\r\n",
                launched.display(),
                recorder.display(),
            ),
        )
        .unwrap();

        let injection_arg = format!("& echo injected > {}", injected.display());
        let expected = vec![
            "".to_string(),
            "contains spaces".to_string(),
            "&|<>^()%PATH%!".to_string(),
            "'single quotes'".to_string(),
            "back\\slash".to_string(),
            "snow-\u{96ea}-fox-\u{72d0}".to_string(),
            injection_arg,
        ];
        let output = run_bounded_process(
            &shim.to_string_lossy(),
            &expected,
            NpmProcessCwd::Trusted(temp.path().to_path_buf()),
            test_limits(Duration::from_secs(5), 16 * 1024),
        )
        .await
        .unwrap();

        assert!(output.status.success(), "shim stderr: {:?}", output.stderr);
        assert_eq!(output.stdout.len() % 2, 0, "cscript emitted partial UTF-16");
        let mut units = output
            .stdout
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        if units.first() == Some(&0xfeff) {
            units.remove(0);
        }
        let mut actual = Vec::new();
        let mut start = 0;
        for (index, unit) in units.iter().enumerate() {
            if *unit == 0 {
                actual.push(String::from_utf16(&units[start..index]).unwrap());
                start = index + 1;
            }
        }
        assert_eq!(
            start,
            units.len(),
            "cscript argv output lacked a terminator"
        );
        assert_eq!(actual, expected);
        assert!(launched.exists(), "batch shim did not run");
        assert!(!injected.exists(), "batch argument executed as a command");

        std::fs::remove_file(&launched).unwrap();
        for unsafe_arg in ["\"double quotes\"", "trailing\\"] {
            let error = run_bounded_process(
                &shim.to_string_lossy(),
                &[unsafe_arg.to_string()],
                NpmProcessCwd::Trusted(temp.path().to_path_buf()),
                test_limits(Duration::from_secs(5), 16 * 1024),
            )
            .await
            .unwrap_err();
            match error {
                NpmProcessError::Spawn(message) => {
                    assert!(message.contains("cannot be represented safely and exactly"));
                }
                error => panic!("unexpected error for unrepresentable batch argv: {error}"),
            }
            assert!(
                !launched.exists(),
                "unrepresentable batch argv unexpectedly launched the shim"
            );
        }
    }

    #[test]
    fn trusted_project_command_precedes_pi_and_global_settings() {
        let rpi = Settings {
            npm_command: Some(strings(&["bun"])),
            ..Settings::default()
        };
        let pi = Settings {
            npm_command: Some(strings(&["pnpm"])),
            ..Settings::default()
        };
        let global = Settings {
            npm_command: Some(strings(&["npm", "--global-prefix"])),
            ..Settings::default()
        };
        assert_eq!(
            select_argv(&[rpi.clone(), pi.clone()], &global, true),
            rpi.npm_command.as_deref()
        );
        assert_eq!(
            select_argv(&[Settings::default(), pi.clone()], &global, true),
            pi.npm_command.as_deref()
        );
        assert_eq!(
            select_argv(&[rpi, pi], &global, false),
            global.npm_command.as_deref()
        );
    }

    #[test]
    fn global_root_output_is_single_absolute_line() {
        let absolute = std::env::temp_dir().join("node_modules");
        let absolute_text = absolute.to_string_lossy().into_owned();
        assert_eq!(
            parse_single_absolute_line(&format!("{absolute_text}\n")).as_deref(),
            Some(absolute.as_path())
        );
        assert!(parse_single_absolute_line("node_modules").is_none());
        assert!(parse_single_absolute_line(&format!(
            "{absolute_text}\n{}",
            std::env::temp_dir().join("other").display()
        ))
        .is_none());
        assert!(parse_single_absolute_line(&format!("{absolute_text}\tother")).is_none());
    }

    #[test]
    fn global_package_path_requires_direct_manifest_child() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("global/node_modules");
        let package = root.join("demo");
        let scoped = root.join("@scope/pkg");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir_all(&scoped).unwrap();
        std::fs::write(package.join("package.json"), "{\"name\":\"demo\"}").unwrap();
        std::fs::write(scoped.join("package.json"), "{\"name\":\"@scope/pkg\"}").unwrap();

        assert_eq!(
            validate_global_package_path(&root, "demo"),
            std::fs::canonicalize(&package).ok()
        );
        assert_eq!(
            validate_global_package_path(&root, "@scope/pkg"),
            std::fs::canonicalize(&scoped).ok()
        );
        assert!(validate_global_package_path(&root, "../outside").is_none());
        assert!(validate_global_package_path(&root, "demo/nested").is_none());
        assert!(validate_global_package_path(&root, "missing").is_none());
    }

    #[test]
    fn pnpm_global_json_requires_root_containment_and_node_modules_shape() {
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("pnpm/global/v11");
        let package = global.join("20-hash/node_modules/demo");
        let outside = temp.path().join("outside/node_modules/demo");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(package.join("package.json"), "{\"name\":\"demo\"}").unwrap();
        std::fs::write(outside.join("package.json"), "{\"name\":\"demo\"}").unwrap();

        let output = serde_json::json!([{
            "path": global,
            "dependencies": {
                "demo": {"path": package}
            }
        }])
        .to_string();
        assert_eq!(
            parse_pnpm_global_package_path(&output, "demo"),
            std::fs::canonicalize(&package).ok()
        );

        let escaped = serde_json::json!([{
            "path": global,
            "dependencies": {
                "demo": {"path": outside}
            }
        }])
        .to_string();
        assert!(parse_pnpm_global_package_path(&escaped, "demo").is_none());
        let missing_root = serde_json::json!([{
            "dependencies": {
                "demo": {"path": package}
            }
        }])
        .to_string();
        assert!(parse_pnpm_global_package_path(&missing_root, "demo").is_none());
        assert!(parse_pnpm_global_package_path("not json", "demo").is_none());
    }

    #[test]
    fn pnpm_global_batch_parses_one_document_and_preserves_path_validation() {
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("pnpm/global/v11");
        let demo = global.join("20-hash/node_modules/demo");
        let scoped = global.join("21-hash/node_modules/@scope/pkg");
        let outside = temp.path().join("outside/node_modules/escaped");
        for package in [&demo, &scoped, &outside] {
            std::fs::create_dir_all(package).unwrap();
            std::fs::write(package.join("package.json"), "{}").unwrap();
        }

        let output = serde_json::json!([{
            "path": global,
            "dependencies": {
                "demo": {"path": demo},
                "@scope/pkg": {"path": scoped},
                "escaped": {"path": outside}
            }
        }])
        .to_string();
        let package_names = strings(&["demo", "@scope/pkg", "escaped", "missing"]);
        let packages = parse_pnpm_global_package_paths(&output, &package_names);

        assert_eq!(packages.len(), 2);
        assert_eq!(
            packages.get("demo"),
            std::fs::canonicalize(&demo).ok().as_ref()
        );
        assert_eq!(
            packages.get("@scope/pkg"),
            std::fs::canonicalize(&scoped).ok().as_ref()
        );
        assert!(!packages.contains_key("escaped"));
        assert!(!packages.contains_key("missing"));
    }

    #[test]
    fn pnpm_global_batch_executes_one_lookup_and_rejects_invalid_names_before_spawn() {
        let temp = tempfile::tempdir().unwrap();
        let global = temp.path().join("pnpm/global/v11");
        let demo = global.join("20-hash/node_modules/demo");
        let scoped = global.join("21-hash/node_modules/@scope/pkg");
        for package in [&demo, &scoped] {
            std::fs::create_dir_all(package).unwrap();
            std::fs::write(package.join("package.json"), "{}").unwrap();
        }
        let output = serde_json::json!([{
            "path": global,
            "dependencies": {
                "demo": {"path": demo},
                "@scope/pkg": {"path": scoped}
            }
        }])
        .to_string();
        let counter = temp.path().join("lookup-count");

        #[cfg(windows)]
        let (program, prefix_args) = {
            let script = temp.path().join("fake-pnpm.ps1");
            std::fs::write(
                &script,
                format!(
                    "[IO.File]::AppendAllText('{}', 'x'); [Console]::Out.Write('{}')",
                    counter.to_string_lossy().replace('\'', "''"),
                    output.replace('\'', "''")
                ),
            )
            .unwrap();
            (
                powershell_program(),
                vec![
                    "-NoLogo".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-File".to_string(),
                    script.to_string_lossy().into_owned(),
                ],
            )
        };
        #[cfg(unix)]
        let (program, prefix_args) = script_command(format!(
            "printf x >> '{}'; printf %s '{}'; exit 0",
            counter.to_string_lossy().replace('\'', "'\"'\"'"),
            output.replace('\'', "'\"'\"'")
        ));
        let command = NpmCommand {
            program,
            prefix_args,
            manager_kind: NpmManagerKind::Pnpm,
            configured: true,
        };

        let packages = command
            .global_package_paths(&strings(&["demo", "@scope/pkg", "missing"]))
            .unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(std::fs::read_to_string(&counter).unwrap(), "x");

        std::fs::remove_file(&counter).unwrap();
        let error = command
            .global_package_paths(&strings(&["demo", "../escaped"]))
            .unwrap_err();
        assert!(error.contains("invalid npm package name"));
        assert!(!counter.exists(), "invalid batch unexpectedly ran pnpm");
    }

    #[cfg(unix)]
    #[test]
    fn global_package_path_rejects_package_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("global/node_modules");
        let outside = temp.path().join("outside/demo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("package.json"), "{\"name\":\"demo\"}").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("demo")).unwrap();
        assert!(validate_global_package_path(&root, "demo").is_none());
    }
}
