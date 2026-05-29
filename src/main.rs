use anyhow::{bail, Context, Result};
use minimoon_sync_server::{preferred_bind_ip, run_server, ServerConfig};
use std::path::PathBuf;
use tokio::sync::oneshot;

#[tokio::main]
async fn main() -> Result<()> {
    let root_dir = parse_root_dir(std::env::args().skip(1))?;
    let ip = preferred_bind_ip()?;
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let config = ServerConfig::new(root_dir.clone());
    let port = config.port;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let server_task = tokio::spawn(run_server(config, shutdown_rx, Some(ready_tx)));

    match ready_rx.await {
        Ok(Ok(addr)) => {
            println!("Sharing directory: {}", root_dir.display());
            println!("Hostname: {hostname}");
            println!("LAN IP: {ip}");
            println!(
                "Enter this address in the iPhone app: http://{}:{}",
                addr.ip(),
                addr.port()
            );
            println!("Press Ctrl-C to stop.");
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

fn parse_root_dir(args: impl IntoIterator<Item = String>) -> Result<PathBuf> {
    let mut args = args.into_iter();
    let Some(path) = args.next() else {
        bail!("usage: minimoon-sync-server <directory>");
    };

    if args.next().is_some() {
        bail!("usage: minimoon-sync-server <directory>");
    }

    let path = PathBuf::from(path);
    if path.as_os_str().is_empty() {
        bail!("directory cannot be empty");
    }

    if !path.is_dir() {
        bail!("directory does not exist: {}", path.display());
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::parse_root_dir;

    #[test]
    fn rejects_missing_directory_argument() {
        let error = parse_root_dir(Vec::new()).unwrap_err();
        assert!(error.to_string().contains("usage"));
    }

    #[test]
    fn rejects_extra_arguments() {
        let error = parse_root_dir(vec![".".to_string(), "extra".to_string()]).unwrap_err();
        assert!(error.to_string().contains("usage"));
    }

    #[test]
    fn rejects_nonexistent_directory() {
        let error =
            parse_root_dir(vec!["/definitely/not/a/minimoon/directory".to_string()]).unwrap_err();
        assert!(error.to_string().contains("does not exist"));
    }
}
