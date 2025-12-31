use clap::Parser;
use ssh_agent_lib::agent::Agent;
use ssh_agent_lib::agent::service_binding::Binding;
use ssh_agent_lib::client::connect;
use ssh_agent_lib::error::AgentError;
use ssh_agent_lib::proto::{Request, Response};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tokio::signal;

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
    /// Path to the socket to bind to
    #[arg(short = 's', long = "sock", value_name = "PATH", required = true)]
    socket: PathBuf,

    /// Path to the openssh ssh-agent binary (optional, defaults to 'ssh-agent' in PATH)
    #[arg(short = 'a', long = "agent", value_name = "PATH")]
    agent_path: Option<PathBuf>,

    /// Additional arguments to pass to the real ssh-agent (everything after '--')
    #[arg(last = true, allow_hyphen_values = true, hide = true)]
    agent_args: Vec<String>,
}

#[derive(Clone)]
struct Proxy {
    backend_socket_path: PathBuf,
}

impl Proxy {
    fn new(backend_socket_path: PathBuf) -> Self {
        Self {
            backend_socket_path,
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
        let backend = connect(
            Binding::FilePath(self.backend_socket_path.clone())
                .try_into()
                .unwrap(),
        )
        .expect("Failed to establish connection to ssh-agent backend");

        ProxySession { backend }
    }
}

#[cfg(windows)]
impl Agent<Listener> for Proxy {
    fn new_session(
        &mut self,
        _: &tokio::net::windows::named_pipe::NamedPipeServer,
    ) -> impl Session {
        let backend = connect(
            Binding::NamedPipe(self.backend_socket_path.clone().into_os_string())
                .try_into()
                .unwrap(),
        )
        .expect("Failed to establish connection to ssh-agent backend");

        ProxySession { backend }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Ensure the parent directory of the socket exists
    if let Some(parent) = args.socket.parent()
        && !parent.exists() {
            fs::create_dir_all(parent)?;
            println!("Created directory: {}", parent.display());
        }

    // Clean up old socket if present
    if args.socket.exists() {
        fs::remove_file(&args.socket)?;
    }

    // Determine ssh-agent binary path
    let agent_binary = args
        .agent_path
        .unwrap_or_else(|| PathBuf::from("ssh-agent"));

    // Build the command for the real ssh-agent
    let mut cmd = Command::new(&agent_binary);
    cmd.args(&args.agent_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("SSH_AUTH_SOCK"); // avoid inheriting SSH_AUTH_SOCK etc.

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn {}: {}", agent_binary.display(), e))?;
    // openssh's ssh-agent will fork into the background so we can directly reap the child here
    let exit_status = child
        .wait()
        .map_err(|e| format!("Failed to wait {}: {}", agent_binary.display(), e))?;
    if !exit_status.success() {
        panic!("ssh-agent returns non-zero exit code");
    }

    // Read the output to extract the real SSH_AUTH_SOCK
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut buffer = String::new();
    stdout.read_to_string(&mut buffer)?;

    let real_sock = buffer
        .lines()
        .find(|line| line.starts_with("SSH_AUTH_SOCK="))
        .and_then(|line| line.split('=').nth(1))
        .and_then(|s| s.split(';').next())
        .ok_or_else(|| format!("Failed to parse SSH_AUTH_SOCK from output:\n{}", buffer))?
        .to_string();

    println!("Real ssh-agent running with socket: {}", real_sock);
    println!("Proxy listening on: {}", args.socket.display());

    let socket_path = args.socket.clone();
    tokio::spawn(async move {
        signal::ctrl_c().await.expect("failed to listen for ctrl+c");
        println!("\nShutting down...");

        // Remove our proxy socket
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
            println!("Removed proxy socket: {}", socket_path.display());
        }

        std::process::exit(0);
    });

    listen(Listener::bind(&args.socket)?, Proxy::new(real_sock.into())).await?;
    Ok(())
}
