mod live;
mod package;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "UvA OpenCode development and release tasks")]
struct Args {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    /// Fetch all supported vendor artifacts and write the reviewed version/hash lock.
    LockArtifacts,
    /// Build the native runtime bundle and check its included executables.
    Bundle,
    /// Build an installer for the host OS and architecture.
    Native,
    /// Create a source archive from explicitly allowed files and directories.
    Source,
    /// Install a self-contained bundle in this account on macOS or Linux.
    Install {
        #[arg(
            long,
            help = "Install under PREFIX/bin and PREFIX/share instead of the default user directories."
        )]
        prefix: Option<PathBuf>,
    },
    /// Exercise the real OpenCode agent in a disposable project through a running Rust bridge.
    Live {
        #[arg(long, default_value = "gpt-6-luna")]
        model: String,
        #[arg(long, value_enum, default_value_t = live::Scenario::Copy)]
        scenario: live::Scenario,
        #[arg(long)]
        state: Option<PathBuf>,
        #[arg(long)]
        opencode: Option<PathBuf>,
    },
    /// Check configured models through a running Rust bridge.
    Models {
        #[arg(long)]
        state: Option<PathBuf>,
    },
}

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[tokio::main]
async fn main() -> Result<()> {
    match Args::parse().command {
        Task::LockArtifacts => package::lock_artifacts().await,
        Task::Bundle => {
            package::bundle().await?;
            Ok(())
        }
        Task::Native => package::native().await,
        Task::Source => package::source(),
        Task::Install { prefix } => package::install(prefix).await,
        Task::Live {
            model,
            scenario,
            state,
            opencode,
        } => live::run(&model, scenario, state, opencode).await,
        Task::Models { state } => live::models(state).await,
    }
}
