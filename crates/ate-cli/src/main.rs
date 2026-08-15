use executor_plugin::{PluginManifest, pack_directory, validate_package};
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() {
    if let Err(error) = run() {
        eprintln!("ate: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) != Some("tool") {
        return Err(usage().into());
    }
    match args.get(1).map(String::as_str) {
        Some("pack") => {
            let source = required(&args, 2, "pack requires a directory")?;
            let output = option(&args, "--output").ok_or("pack requires --output <file>")?;
            pack_directory(Path::new(source), File::create(output)?)?;
        }
        Some("validate") => {
            let source = Path::new(required(
                &args,
                2,
                "validate requires a directory or .atepkg",
            )?);
            let manifest = validate_source(source)?;
            println!("{}", serde_json::to_string_pretty(&manifest)?);
        }
        Some("install") => install(&args)?,
        Some(operation @ ("list" | "enable" | "disable" | "remove")) => manage(operation, &args)?,
        _ => return Err(usage().into()),
    }
    Ok(())
}

fn install(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let source = PathBuf::from(required(
        args,
        2,
        "install requires a directory or .atepkg",
    )?);
    validate_source(&source)?;
    let distribution = option(args, "--distribution").unwrap_or("Ubuntu");
    let replace = args.iter().any(|value| value == "--replace");
    let command = if replace {
        "exec \"$HOME/.local/bin/ate-daemon\" tool install --replace"
    } else {
        "exec \"$HOME/.local/bin/ate-daemon\" tool install"
    };
    let mut child = wsl_command(distribution, command)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    {
        let mut stdin = child.stdin.take().ok_or("could not open WSL stdin")?;
        if source.is_dir() {
            pack_directory(&source, &mut stdin)?;
        } else {
            io::copy(&mut File::open(&source)?, &mut stdin)?;
        }
        stdin.flush()?;
    }
    ensure_success(child.wait()?)
}

fn manage(operation: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let distribution = option(args, "--distribution").unwrap_or("Ubuntu");
    let id = if operation == "list" {
        None
    } else {
        Some(required(args, 2, "operation requires a plugin id")?)
    };
    if let Some(id) = id {
        validate_cli_id(id)?;
    }
    let command = match id {
        Some(id) => format!("exec \"$HOME/.local/bin/ate-daemon\" tool {operation} {id}"),
        None => format!("exec \"$HOME/.local/bin/ate-daemon\" tool {operation}"),
    };
    ensure_success(wsl_command(distribution, &command).status()?)
}

fn validate_source(path: &Path) -> Result<PluginManifest, Box<dyn std::error::Error>> {
    if path.is_dir() {
        let raw = std::fs::read_to_string(path.join("tool.json"))?;
        Ok(PluginManifest::parse(&raw).map_err(|error| format!("invalid manifest: {error}"))?)
    } else {
        Ok(validate_package(File::open(path)?)?)
    }
}

fn wsl_command(distribution: &str, script: &str) -> Command {
    let mut command = Command::new("wsl.exe");
    command.args([
        "--distribution",
        distribution,
        "--exec",
        "sh",
        "-lc",
        script,
    ]);
    command
}

fn ensure_success(status: std::process::ExitStatus) -> Result<(), Box<dyn std::error::Error>> {
    if status.success() {
        Ok(())
    } else {
        Err(format!("WSL command exited with {status}").into())
    }
}

fn required<'a>(
    args: &'a [String],
    index: usize,
    message: &str,
) -> Result<&'a str, Box<dyn std::error::Error>> {
    args.get(index)
        .map(String::as_str)
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| message.into())
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

fn validate_cli_id(value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let valid = value.len() <= 128
        && value.split('.').count() >= 2
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err("invalid plugin id".into())
    }
}

fn usage() -> &'static str {
    "usage: ate tool <pack|validate|install|list|enable|disable|remove> ..."
}
