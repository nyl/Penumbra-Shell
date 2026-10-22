use greetd_ipc::{codec::SyncCodec, AuthMessageType, Request, Response};
use std::env;
use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

const SESSION_COMMAND: &str = "/usr/local/bin/penumbra-session";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    clear_screen();
    println!("Penumbra Mobile");
    println!();

    let username = env::var("PENUMBRA_LOGIN_USER")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| prompt("Username: "));
    let socket = env::var("GREETD_SOCK")?;
    let mut stream = UnixStream::connect(socket)?;

    Request::CreateSession {
        username: username.clone(),
    }
    .write_to(&mut stream)?;

    loop {
        match Response::read_from(&mut stream)? {
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                let response = match auth_message_type {
                    AuthMessageType::Info => {
                        println!("{auth_message}");
                        None
                    }
                    AuthMessageType::Error => {
                        eprintln!("{auth_message}");
                        None
                    }
                    AuthMessageType::Visible => Some(prompt(&auth_message)),
                    AuthMessageType::Secret => Some(prompt_secret(&auth_message)),
                };
                Request::PostAuthMessageResponse { response }.write_to(&mut stream)?;
            }
            Response::Success => break,
            Response::Error {
                error_type: _,
                description,
            } => return Err(format!("Login failed: {description}").into()),
        }
    }

    Request::StartSession {
        cmd: vec![SESSION_COMMAND.to_string()],
        env: vec![
            "XDG_CURRENT_DESKTOP=Penumbra".to_string(),
            "XDG_SESSION_DESKTOP=Penumbra".to_string(),
            "XDG_SESSION_TYPE=wayland".to_string(),
        ],
    }
    .write_to(&mut stream)?;

    match Response::read_from(&mut stream)? {
        Response::Success => Ok(()),
        Response::Error {
            error_type: _,
            description,
        } => Err(format!("Could not start Penumbra: {description}").into()),
        Response::AuthMessage { .. } => Err("Unexpected authentication message".into()),
    }
}

fn prompt(message: &str) -> String {
    print!("{message}");
    let _ = io::stdout().flush();
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .expect("failed to read input");
    value.trim_end_matches(['\r', '\n']).to_string()
}

fn prompt_secret(message: &str) -> String {
    print!("{message}");
    let _ = io::stdout().flush();
    let terminal = Path::new("/dev/tty");
    let _ = set_echo(terminal, false);
    let value = prompt("");
    let _ = set_echo(terminal, true);
    println!();
    value
}

fn set_echo(path: &Path, enabled: bool) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
    let mut termios = unsafe {
        let mut value = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut value) != 0 {
            return Err(io::Error::last_os_error());
        }
        value
    };
    if enabled {
        termios.c_lflag |= libc::ECHO;
    } else {
        termios.c_lflag &= !libc::ECHO;
    }
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn clear_screen() {
    print!("\x1b[2J\x1b[H");
    let _ = io::stdout().flush();
}
