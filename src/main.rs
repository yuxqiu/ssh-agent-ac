use clap::Parser;
use ssh_agent_lib::agent::Agent;
use ssh_agent_lib::agent::service_binding::Binding;
use ssh_agent_lib::client::connect;
use ssh_agent_lib::error::AgentError;
use ssh_agent_lib::proto::{Request, Response};
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::signal;
use tokio::sync::watch;
use tokio::process::Command;

#[cfg(windows)]
use ssh_agent_lib::agent::NamedPipeListener as Listener;
#[cfg(not(windows))]
use tokio::net::UnixListener as Listener;

use ssh_agent_lib::proto::message::KeyConstraint;
use ssh_agent_lib::{
    agent::{Session, listen},
    proto::AddIdentityConstrained,
};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "SSH agent wrapper that forces confirm constraint on key additions"
)]
struct Args {
    /// Path to the socket to bind to (optional, defaults to a temp path)
    #[arg(short = 's', long = "sock", value_name = "PATH")]
    socket: Option<PathBuf>,

    /// Command to run with SSH_AUTH_SOCK redirected through the proxy
    #[arg(value_name = "BIN")]
    bin: String,

    /// Arguments for the command
    #[arg(value_name = "ARGS", trailing_var_arg = true, allow_hyphen_values = true)]
    bin_args: Vec<String>,
}

#[derive(Clone)]
struct Proxy {
    backend_socket_path: PathBuf,
    fatal_tx: watch::Sender<bool>,
}

impl Proxy {
    fn new(backend_socket_path: PathBuf, fatal_tx: watch::Sender<bool>) -> Self {
        Self {
            backend_socket_path,
            fatal_tx,
        }
    }
}

struct ProxySession {
    backend: Box<dyn Session>,
}

#[ssh_agent_lib::async_trait]
impl Session for ProxySession {
    async fn handle(&mut self, message: Request) -> Result<Response, AgentError> {
        match message {
            Request::AddIdentity(add) => {
                // Rewrite to constrained add with confirm
                let constrained = AddIdentityConstrained {
                    identity: add,
                    constraints: vec![KeyConstraint::Confirm],
                };
                self.backend
                    .handle(Request::AddIdConstrained(constrained))
                    .await
            }
            Request::AddIdConstrained(mut add_con) => {
                // Ensure confirm constraint is present
                if !add_con
                    .constraints
                    .iter()
                    .any(|c| matches!(c, KeyConstraint::Confirm))
                {
                    add_con.constraints.push(KeyConstraint::Confirm);
                }
                self.backend
                    .handle(Request::AddIdConstrained(add_con))
                    .await
            }
            // Forward everything else unchanged
            msg => self.backend.handle(msg).await,
        }
    }
}

#[cfg(unix)]
impl Agent<Listener> for Proxy {
    fn new_session(&mut self, _: &tokio::net::UnixStream) -> impl Session {
        let binding = Binding::FilePath(self.backend_socket_path.clone())
            .try_into()
            .unwrap_or_else(|e| {
                let _ = self.fatal_tx.send(true);
                panic!("Failed to create binding for ssh-agent backend: {e}");
            });
        let backend = connect(binding).unwrap_or_else(|e| {
            let _ = self.fatal_tx.send(true);
            panic!("Failed to establish connection to ssh-agent backend: {e}");
        });

        ProxySession { backend }
    }
}

#[cfg(windows)]
impl Agent<Listener> for Proxy {
    fn new_session(
        &mut self,
        _: &tokio::net::windows::named_pipe::NamedPipeServer,
    ) -> impl Session {
        let binding = Binding::NamedPipe(self.backend_socket_path.clone().into_os_string())
            .try_into()
            .unwrap_or_else(|e| {
                let _ = self.fatal_tx.send(true);
                panic!("Failed to create binding for ssh-agent backend: {e}");
            });
        let backend = connect(binding).unwrap_or_else(|e| {
            let _ = self.fatal_tx.send(true);
            panic!("Failed to establish connection to ssh-agent backend: {e}");
        });

        ProxySession { backend }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let backend_socket = std::env::var_os("SSH_AUTH_SOCK")
        .ok_or("Missing SSH_AUTH_SOCK for backend ssh-agent socket.")?;
    let backend_socket_path = PathBuf::from(backend_socket);

    let socket = args.socket.unwrap_or_else(|| {
        let mut path = std::env::temp_dir();
        path.push(format!("ssh-agent-ac-{}.sock", std::process::id()));
        path
    });

    // Ensure the parent directory of the socket exists
    if let Some(parent) = socket.parent()
        && !parent.exists() {
            fs::create_dir_all(parent)?;
            println!("Created directory: {}", parent.display());
        }

    // Clean up old socket if present
    if socket.exists() {
        fs::remove_file(&socket)?;
    }

    println!("Backend ssh-agent socket: {}", backend_socket_path.display());
    println!("Proxy listening on: {}", socket.display());

    let mut cmd = Command::new(&args.bin);
    cmd.args(&args.bin_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env("SSH_AUTH_SOCK", &socket);

    let (fatal_tx, mut fatal_rx) = watch::channel(false);

    let server_socket = socket.clone();
    let listener = Listener::bind(&server_socket)?;
    let server = tokio::spawn(async move {
        listen(listener, Proxy::new(backend_socket_path, fatal_tx)).await
    });
    tokio::pin!(server);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            server.abort();
            if socket.exists() {
                let _ = fs::remove_file(&socket);
            }
            return Err(format!("Failed to spawn {}: {}", args.bin, e).into());
        }
    };

    let mut fatal_rx_active = true;
    let child_status = loop {
        tokio::select! {
            status = child.wait() => break status?,
            result = &mut server => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                if socket.exists() {
                    let _ = fs::remove_file(&socket);
                }
                return Err(format!("Proxy exited unexpectedly: {:?}", result).into());
            },
            result = fatal_rx.changed(), if fatal_rx_active => {
                if result.is_ok() && *fatal_rx.borrow() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    if socket.exists() {
                        let _ = fs::remove_file(&socket);
                    }
                    return Err("Proxy aborted due to backend connection failure.".into());
                }
                if result.is_err() {
                    fatal_rx_active = false;
                }
                continue;
            },
            _ = signal::ctrl_c() => {
                let _ = child.kill().await;
                break child.wait().await?;
            }
        }
    };

    server.abort();
    if socket.exists() {
        let _ = fs::remove_file(&socket);
    }

    if !child_status.success() {
        return Err(format!("Command exited with status: {}", child_status).into());
    }

    Ok(())
}
