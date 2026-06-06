use anyhow::{bail, Context, Result};
use minimoon_sync_server::{
    list_files_with_options, preferred_bind_ip, run_server, FileAccessOptions, FileInfo,
    ServerConfig,
};
use std::io::IsTerminal;
use std::path::PathBuf;
use tokio::sync::oneshot;
use tracing_subscriber::filter::LevelFilter;

const USAGE: &str =
    "usage: minimoon-sync-server [--verbose] [--allow-top-level-directory-symlinks] <directory>";
const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_GREEN: &str = "\x1b[32m";

#[derive(Debug)]
struct CliOptions {
    root_dir: PathBuf,
    verbose: bool,
    allow_top_level_directory_symlinks: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let options = parse_args(std::env::args().skip(1))?;

    tracing_subscriber::fmt()
        .with_max_level(if options.verbose {
            LevelFilter::DEBUG
        } else {
            LevelFilter::OFF
        })
        .init();

    let root_dir = options.root_dir;
    let ip = preferred_bind_ip()?;
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let file_access_options = FileAccessOptions {
        allow_top_level_directory_symlinks: options.allow_top_level_directory_symlinks,
    };
    let shared_files = list_files_with_options(root_dir.clone(), file_access_options)
        .context("failed to list shared files")?;
    let shared_summary = summarize_files(&shared_files);
    let config = ServerConfig::new(root_dir.clone())
        .with_allow_top_level_directory_symlinks(options.allow_top_level_directory_symlinks);
    let port = config.port;
    let use_color = stdout_supports_color();

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let server_task = tokio::spawn(run_server(config, shutdown_rx, Some(ready_tx)));

    match ready_rx.await {
        Ok(Ok(addr)) => {
            println!("{}", startup_title(use_color));
            println!(
                "{}",
                startup_table(
                    &[
                        ("Directory", root_dir.display().to_string()),
                        ("Files", format_count(shared_summary.file_count)),
                        ("Total size", format_file_size(shared_summary.total_size)),
                        ("Hostname", hostname),
                        ("LAN IP", ip.to_string()),
                        (
                            "App address",
                            format!("http://{}:{}", addr.ip(), addr.port()),
                        ),
                    ],
                    use_color
                )
            );
            println!("{}", paint("Press Ctrl-C to stop.", ANSI_DIM, use_color));
        }
        Ok(Err(error)) => bail!(error),
        Err(_) => bail!("server stopped before reporting readiness"),
    }

    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for Ctrl-C")?;
    let _ = shutdown_tx.send(true);
    server_task.await??;
    println!("Stopped sharing on port {port}.");

    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<CliOptions> {
    let mut root_dir = None;
    let mut verbose = false;
    let mut allow_top_level_directory_symlinks = false;

    for arg in args {
        match arg.as_str() {
            "--verbose" | "-v" => verbose = true,
            "--allow-top-level-directory-symlinks" => {
                allow_top_level_directory_symlinks = true;
            }
            _ if arg.starts_with('-') => bail!(USAGE),
            _ => {
                if root_dir.replace(PathBuf::from(arg)).is_some() {
                    bail!(USAGE);
                }
            }
        }
    }

    let Some(path) = root_dir else {
        bail!(USAGE);
    };

    if path.as_os_str().is_empty() {
        bail!("directory cannot be empty");
    }

    if !path.is_dir() {
        bail!("directory does not exist: {}", path.display());
    }

    Ok(CliOptions {
        root_dir: path,
        verbose,
        allow_top_level_directory_symlinks,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct FileSummary {
    file_count: usize,
    total_size: u64,
}

fn summarize_files(files: &[FileInfo]) -> FileSummary {
    FileSummary {
        file_count: files.len(),
        total_size: files.iter().map(|file| file.size).sum(),
    }
}

fn format_file_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];

    if bytes < 1024 {
        let suffix = if bytes == 1 { "byte" } else { "bytes" };
        return format!("{bytes} {suffix}");
    }

    let mut size = bytes as f64;
    let mut unit_index = 0;
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    format!("{size:.1} {}", UNITS[unit_index])
}

fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);

    for (index, digit) in digits.chars().enumerate() {
        let remaining = digits.len() - index;
        if index > 0 && remaining.is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(digit);
    }

    formatted
}

fn stdout_supports_color() -> bool {
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn startup_title(use_color: bool) -> String {
    paint("Minimoon Sync Server", ANSI_BOLD, use_color)
}

fn paint(text: impl AsRef<str>, code: &str, use_color: bool) -> String {
    if use_color {
        format!("{code}{}{ANSI_RESET}", text.as_ref())
    } else {
        text.as_ref().to_string()
    }
}

fn startup_table(rows: &[(&str, String)], use_color: bool) -> String {
    let label_width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    let value_width = rows.iter().map(|(_, value)| value.len()).max().unwrap_or(0);
    let border = format!(
        "+{}+{}+",
        "-".repeat(label_width + 2),
        "-".repeat(value_width + 2)
    );
    let border = paint(border, ANSI_DIM, use_color);

    let mut lines = Vec::with_capacity(rows.len() + 3);
    lines.push(border.clone());
    for (label, value) in rows {
        let is_app_address = *label == "App address";
        let label = paint(format!("{label:<label_width$}"), ANSI_CYAN, use_color);
        let value_code = if is_app_address {
            ANSI_GREEN
        } else {
            ANSI_BOLD
        };
        let value = paint(format!("{value:<value_width$}"), value_code, use_color);
        lines.push(format!("| {label} | {value} |"));
    }
    lines.push(border);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        format_count, format_file_size, parse_args, startup_table, startup_title, summarize_files,
    };
    use minimoon_sync_server::FileInfo;

    #[test]
    fn rejects_missing_directory_argument() {
        let error = parse_args(Vec::new()).unwrap_err();
        assert!(error.to_string().contains("usage"));
    }

    #[test]
    fn rejects_extra_arguments() {
        let error = parse_args(vec![".".to_string(), "extra".to_string()]).unwrap_err();
        assert!(error.to_string().contains("usage"));
    }

    #[test]
    fn rejects_nonexistent_directory() {
        let error =
            parse_args(vec!["/definitely/not/a/minimoon/directory".to_string()]).unwrap_err();
        assert!(error.to_string().contains("does not exist"));
    }

    #[test]
    fn parses_verbose_flag() {
        let options = parse_args(vec!["--verbose".to_string(), ".".to_string()]).unwrap();
        assert!(options.verbose);
        assert_eq!(options.root_dir, std::path::PathBuf::from("."));
    }

    #[test]
    fn parses_top_level_directory_symlink_flag() {
        let options = parse_args(vec![
            "--allow-top-level-directory-symlinks".to_string(),
            ".".to_string(),
        ])
        .unwrap();
        assert!(options.allow_top_level_directory_symlinks);
        assert_eq!(options.root_dir, std::path::PathBuf::from("."));
    }

    #[test]
    fn rejects_unknown_flags() {
        let error = parse_args(vec!["--unknown".to_string(), ".".to_string()]).unwrap_err();
        assert!(error.to_string().contains("usage"));
    }

    #[test]
    fn summarizes_shared_files() {
        let files = vec![
            FileInfo {
                path: "track.mp3".to_string(),
                size: 5,
                last_modified: 0,
            },
            FileInfo {
                path: "cover.jpg".to_string(),
                size: 7,
                last_modified: 0,
            },
        ];

        let summary = summarize_files(&files);

        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.total_size, 12);
    }

    #[test]
    fn formats_file_sizes() {
        assert_eq!(format_file_size(0), "0 bytes");
        assert_eq!(format_file_size(1), "1 byte");
        assert_eq!(format_file_size(1024), "1.0 KiB");
        assert_eq!(format_file_size(1536), "1.5 KiB");
        assert_eq!(format_file_size(1_610_612_736), "1.5 GiB");
    }

    #[test]
    fn formats_counts() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_count(1_234), "1,234");
        assert_eq!(format_count(1_234_567), "1,234,567");
    }

    #[test]
    fn formats_startup_table() {
        let table = startup_table(
            &[
                ("Directory", "/music".to_string()),
                ("Files", format_count(12_345)),
                ("App address", "http://192.168.1.44:41324".to_string()),
            ],
            false,
        );

        assert_eq!(
            table,
            "+-------------+---------------------------+\n\
             | Directory   | /music                    |\n\
             | Files       | 12,345                    |\n\
             | App address | http://192.168.1.44:41324 |\n\
             +-------------+---------------------------+"
        );
    }

    #[test]
    fn formats_colored_startup_output() {
        assert_eq!(startup_title(true), "\x1b[1mMinimoon Sync Server\x1b[0m");

        let table = startup_table(
            &[("App address", "http://localhost:41324".to_string())],
            true,
        );

        assert!(table.contains("\x1b[2m+"));
        assert!(table.contains("\x1b[36mApp address\x1b[0m"));
        assert!(table.contains("\x1b[32mhttp://localhost:41324\x1b[0m"));
    }
}
