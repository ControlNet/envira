use std::{
    env,
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;

use envira::{
    app::App,
    cli::Cli,
    engine::{
        CommandErrorResponse, CommandRequest, Engine, EngineError, InterfaceMode, OutputFormat,
        VersionGate, CURRENT_VERSION_ENV,
    },
    error::Result,
};

const DEFAULT_UPDATE_WRAPPER_URL: &str = "https://boot.controlnet.space/envira";
const UPDATE_WRAPPER_PATH_ENV: &str = "ENVIRA_UPDATE_WRAPPER_PATH";
const UPDATE_WRAPPER_URL_ENV: &str = "ENVIRA_UPDATE_WRAPPER_URL";
const WRAPPER_HANDOFF_MARKER: &str = "Handing off to";

fn main() {
    match run() {
        Ok(exit_code) if exit_code != 0 => std::process::exit(exit_code),
        Ok(_) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<i32> {
    let argv = env::args_os().collect::<Vec<_>>();
    let cli = Cli::parse_from(argv.clone());
    let request = cli.into_request();
    let engine = Engine::default();

    match engine.assess_version_gate(&request) {
        Ok(VersionGate::NotApplicable | VersionGate::Satisfied) => {}
        Ok(VersionGate::UpdateRequired {
            current_version,
            required_version,
        }) => {
            return handoff_to_updater(
                &request,
                &argv[1..],
                current_version.as_str(),
                required_version.as_str(),
            );
        }
        Err(error) => return emit_engine_error(&request, error),
    }

    App::default().run(request)
}

fn handoff_to_updater(
    request: &CommandRequest,
    raw_args: &[OsString],
    current_version: &str,
    required_version: &str,
) -> Result<i32> {
    let wrapper = match resolve_update_wrapper() {
        Ok(wrapper) => wrapper,
        Err(detail) => {
            return emit_engine_error(
                request,
                EngineError::AutoUpdateFailed {
                    current_version: current_version.to_string(),
                    required_version: required_version.to_string(),
                    updater: "envira.sh".to_string(),
                    detail,
                    exit_code: None,
                },
            );
        }
    };

    let mut updater = Command::new("bash");
    updater
        .env_remove(CURRENT_VERSION_ENV)
        .arg(wrapper.path())
        .arg("--run")
        .arg("--")
        .args(raw_args);

    let output = match updater.output() {
        Ok(output) => output,
        Err(error) => {
            return emit_engine_error(
                request,
                EngineError::AutoUpdateFailed {
                    current_version: current_version.to_string(),
                    required_version: required_version.to_string(),
                    updater: wrapper.source().to_string(),
                    detail: format!("failed to launch approved update wrapper: {error}"),
                    exit_code: None,
                },
            );
        }
    };

    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() || stderr.contains(WRAPPER_HANDOFF_MARKER) {
        forward_update_output(&output.stdout, &output.stderr)?;
        return Ok(output.status.code().unwrap_or(1));
    }

    emit_engine_error(
        request,
        EngineError::AutoUpdateFailed {
            current_version: current_version.to_string(),
            required_version: required_version.to_string(),
            updater: wrapper.source().to_string(),
            detail: summarize_update_failure(&output.stderr),
            exit_code: output.status.code(),
        },
    )
}

fn emit_engine_error(request: &CommandRequest, error: EngineError) -> Result<i32> {
    let response = CommandErrorResponse::new(
        request.command,
        request.mode,
        request.format,
        error.into_envelope(),
    );

    match request.mode {
        InterfaceMode::Headless => match request.format {
            OutputFormat::Json => println!("{}", response.as_json()?),
            OutputFormat::Text => println!("{}", render_error_response(&response)),
        },
        InterfaceMode::Tui => eprintln!("{}", render_error_response(&response)),
    }

    Ok(1)
}

fn render_error_response(response: &CommandErrorResponse) -> String {
    response.render_text()
}

fn forward_update_output(stdout: &[u8], stderr: &[u8]) -> Result<()> {
    let mut stdout_handle = io::stdout().lock();
    stdout_handle.write_all(stdout)?;
    stdout_handle.flush()?;

    let mut stderr_handle = io::stderr().lock();
    stderr_handle.write_all(stderr)?;
    stderr_handle.flush()?;

    Ok(())
}

fn summarize_update_failure(stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr);
    let summary = detail
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .find(|line| line.starts_with("[ERROR]"))
        .or_else(|| {
            detail
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .next_back()
        })
        .map(str::to_string);

    if let Some(summary) = summary {
        summary
    } else {
        "the updater exited before handing off to a refreshed envira binary".to_string()
    }
}

fn resolve_update_wrapper() -> std::result::Result<UpdateWrapper, String> {
    if let Some(path) = env::var_os(UPDATE_WRAPPER_PATH_ENV) {
        return Ok(UpdateWrapper::local(PathBuf::from(path)));
    }

    let url =
        env::var(UPDATE_WRAPPER_URL_ENV).unwrap_or_else(|_| DEFAULT_UPDATE_WRAPPER_URL.to_string());
    download_update_wrapper(url.as_str())
}

fn download_update_wrapper(url: &str) -> std::result::Result<UpdateWrapper, String> {
    let temp_dir = unique_temp_dir("update-wrapper")?;
    let wrapper_path = temp_dir.join("envira.sh");
    let wrapper_path_string = wrapper_path.display().to_string();

    match Command::new("curl")
        .arg("--fail")
        .arg("--location")
        .arg("--silent")
        .arg("--show-error")
        .arg(url)
        .arg("--output")
        .arg(wrapper_path_string.as_str())
        .output()
    {
        Ok(output) if output.status.success() => {
            return Ok(UpdateWrapper::downloaded(
                wrapper_path,
                temp_dir,
                url.to_string(),
            ));
        }
        Ok(output) => {
            let detail = summarize_update_failure(&output.stderr);
            return Err(format!(
                "failed to download approved update wrapper from {url}: {detail}"
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(format!(
                "failed to launch curl while downloading approved update wrapper from {url}: {error}"
            ));
        }
        Err(_) => {}
    }

    match Command::new("wget")
        .arg("--quiet")
        .arg(format!("--output-document={wrapper_path_string}"))
        .arg(url)
        .output()
    {
        Ok(output) if output.status.success() => Ok(UpdateWrapper::downloaded(
            wrapper_path,
            temp_dir,
            url.to_string(),
        )),
        Ok(output) => {
            let detail = summarize_update_failure(&output.stderr);
            Err(format!(
                "failed to download approved update wrapper from {url}: {detail}"
            ))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(
            "neither curl nor wget is installed; cannot launch the approved envira update flow"
                .to_string(),
        ),
        Err(error) => Err(format!(
            "failed to launch wget while downloading approved update wrapper from {url}: {error}"
        )),
    }
}

fn unique_temp_dir(label: &str) -> std::result::Result<PathBuf, String> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock error while preparing update wrapper: {error}"))?
        .as_nanos();
    let path = env::temp_dir().join(format!("envira-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&path)
        .map_err(|error| format!("failed to create temporary update wrapper directory: {error}"))?;
    Ok(path)
}

struct UpdateWrapper {
    path: PathBuf,
    source: String,
    cleanup_dir: Option<PathBuf>,
}

impl UpdateWrapper {
    fn local(path: PathBuf) -> Self {
        let source = path.display().to_string();
        Self {
            path,
            source,
            cleanup_dir: None,
        }
    }

    fn downloaded(path: PathBuf, cleanup_dir: PathBuf, source: String) -> Self {
        Self {
            path,
            source,
            cleanup_dir: Some(cleanup_dir),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn source(&self) -> &str {
        &self.source
    }
}

impl Drop for UpdateWrapper {
    fn drop(&mut self) {
        if let Some(cleanup_dir) = &self.cleanup_dir {
            let _ = fs::remove_dir_all(cleanup_dir);
        }
    }
}
