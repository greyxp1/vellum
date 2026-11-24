use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;
use std::time::Instant;

use clap::Parser;

mod cli;
mod config;
mod draw;
mod event_loop;
mod ipc;
mod render;
mod state;
mod text;
mod tool;

use cli::{Cli, Command};

pub(crate) type Rgba = [f32; 4];
pub(crate) type OutputId = u32;

pub(crate) fn color_to_srgb(color: color::DynamicColor) -> Rgba {
    color.convert(color::ColorSpaceTag::Srgb).clip().components
}

fn main() -> ExitCode {
    match run() {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("vellum: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let arguments = Cli::parse();
    if let Some(command) = &arguments.command {
        return run_command(command);
    }
    let mut config = config::Config::new(arguments)?;
    let started = Instant::now();
    // JOURNAL_STREAM can be inherited after stderr was redirected. Match the
    // descriptor before emitting the journal's severity prefixes.
    let journal = std::env::var("JOURNAL_STREAM")
        .ok()
        .zip(std::fs::metadata("/proc/self/fd/2").ok())
        .is_some_and(|(stream, metadata)| {
            stream == format!("{}:{}", metadata.dev(), metadata.ino())
        });
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,vellum=info"))
        .format(move |buf, record| {
            if journal {
                let priority = match record.level() {
                    log::Level::Error => 3,
                    log::Level::Warn => 4,
                    log::Level::Info => 6,
                    log::Level::Debug | log::Level::Trace => 7,
                };
                write!(buf, "<{priority}>")?;
            }
            writeln!(
                buf,
                "[{:.3}s {} {}] {}",
                started.elapsed().as_secs_f64(),
                record.level(),
                record.target(),
                record.args()
            )
        })
        .init();
    config.watch();
    let settings = config.load()?;
    log::info!("Vellum {} starting", env!("CARGO_PKG_VERSION"));
    event_loop::run(settings, config)?;
    Ok(ExitCode::SUCCESS)
}

fn run_command(command: &Command) -> Result<ExitCode, String> {
    let Command::Status {
        field, tool, check, ..
    } = command
    else {
        let reply = matches!(command, Command::Set { .. } | Command::Use { .. });
        return match ipc::send(command, reply)? {
            None if reply => Err("overlay is not running".into()),
            Some(response) if response != "ok" => Err("invalid command response".into()),
            _ => Ok(ExitCode::SUCCESS),
        };
    };
    if tool.is_some() && field.is_some_and(|field| !field.is_tool_field()) {
        return Err("--target requires a tool property".into());
    }
    if *check && field.is_none_or(|field| !field.is_boolean()) {
        return Err("--check requires a boolean field".into());
    }
    let response = ipc::send(command, true)?;
    if *check {
        return match response.as_deref() {
            Some("true") => Ok(ExitCode::SUCCESS),
            Some("false") | None => Ok(ExitCode::from(1)),
            Some("null") => Err("field is not applicable to this tool".into()),
            _ => Err("invalid boolean status response".into()),
        };
    }
    let response = response.ok_or("overlay is not running")?;
    println!("{}", response.strip_suffix('\n').unwrap_or(&response));
    Ok(ExitCode::SUCCESS)
}
