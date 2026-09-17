use orchard_server::ui_router;
use orchard_workspace_host::WorkspaceHost;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Debug, PartialEq)]
struct Options {
    data_dir: PathBuf,
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("orchard: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let Some(options) = parse_args(env::args_os().skip(1))? else {
        println!("{}", usage());
        return Ok(());
    };
    let executable =
        env::current_exe().map_err(|error| format!("cannot locate executable: {error}"))?;
    let br_path = bundled_br_for(&executable)?;
    let host = Arc::new(
        WorkspaceHost::open_with_port(options.data_dir, br_path, options.port)
            .map_err(|error| format!("cannot open workspace host: {error}"))?,
    );
    let server = host
        .clone()
        .start_server_with_ui(ui_router())
        .await
        .map_err(|error| format!("cannot start local server: {error}"))?;
    println!("Orchard is running at http://{}/", server.endpoint());
    println!("Press Ctrl-C to stop.");

    tokio::signal::ctrl_c()
        .await
        .map_err(|error| format!("cannot listen for Ctrl-C: {error}"))?;
    server
        .shutdown()
        .await
        .map_err(|error| format!("server shutdown failed: {error}"))
}

fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Option<Options>, String> {
    let mut arguments = arguments.into_iter();
    let mut data_dir = None;
    let mut port = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("serve") => {}
            Some("--data-dir") => {
                let value = arguments
                    .next()
                    .ok_or_else(|| "--data-dir requires a path".to_owned())?;
                if data_dir.replace(PathBuf::from(value)).is_some() {
                    return Err("--data-dir may be specified only once".to_owned());
                }
            }
            Some("--port") => {
                let value = arguments
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| "--port requires a number from 1 to 65535".to_owned())?;
                let value = value
                    .parse::<u16>()
                    .map_err(|_| "--port requires a number from 1 to 65535".to_owned())?;
                if value == 0 {
                    return Err("--port requires a number from 1 to 65535".to_owned());
                }
                if port.replace(value).is_some() {
                    return Err("--port may be specified only once".to_owned());
                }
            }
            Some("-h" | "--help") => return Ok(None),
            Some(value) => return Err(format!("unknown argument {value:?}\n{}", usage())),
            None => {
                return Err(
                    "arguments must be valid UTF-8 except for the data directory".to_owned(),
                )
            }
        }
    }
    Ok(Some(Options {
        data_dir: data_dir.ok_or_else(|| format!("--data-dir is required\n{}", usage()))?,
        port,
    }))
}

fn bundled_br_for(executable: &Path) -> Result<PathBuf, String> {
    let directory = executable
        .parent()
        .ok_or_else(|| "executable has no parent directory".to_owned())?;
    let candidate = directory.join("resources/bin/br");
    if !candidate.is_file() {
        return Err(format!(
            "bundled task tool is missing at {}; reinstall the Orchard package",
            candidate.display()
        ));
    }
    candidate
        .canonicalize()
        .map_err(|error| format!("cannot resolve bundled task tool: {error}"))
}

fn usage() -> &'static str {
    "usage: orchard [serve] --data-dir PATH [--port PORT]"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parses_required_data_directory_and_optional_port() {
        assert_eq!(
            parse_args(
                ["serve", "--data-dir", "/tmp/orchard data", "--port", "4123"].map(OsString::from)
            )
            .unwrap(),
            Some(Options {
                data_dir: PathBuf::from("/tmp/orchard data"),
                port: Some(4123)
            })
        );
        assert_eq!(parse_args(["--help"].map(OsString::from)).unwrap(), None);
        assert!(parse_args(["--port", "0"].map(OsString::from)).is_err());
        assert!(parse_args(["--data-dir", "/tmp/a", "extra"].map(OsString::from)).is_err());
    }

    #[test]
    fn resolves_only_the_packaged_resource_not_path() {
        let temporary = TempDir::new().unwrap();
        let executable = temporary.path().join("orchard");
        fs::write(&executable, b"binary").unwrap();
        let br = temporary.path().join("resources/bin/br");
        fs::create_dir_all(br.parent().unwrap()).unwrap();
        fs::write(&br, b"tool").unwrap();
        let resolved = bundled_br_for(&executable).unwrap();
        assert_eq!(resolved, br.canonicalize().unwrap());
    }
}
