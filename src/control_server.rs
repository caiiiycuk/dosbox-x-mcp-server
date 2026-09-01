use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{TcpListener, TcpStream, tcp::OwnedReadHalf},
    sync::{mpsc, oneshot},
    time::timeout,
};
use tracing::{error, info, warn};

pub const CONTROL_ADDR: &str = "127.0.0.1:58991";

const CHANNEL_SIZE: usize = 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct ControlServer {
    request_tx: mpsc::Sender<ControlRequest>,
    next_id: Arc<AtomicU64>,
}

#[derive(Debug)]
pub struct ControlResponse {
    pub ok: bool,
    pub lines: Vec<String>,
}

impl ControlResponse {
    pub fn into_text(self) -> String {
        let text = self.lines.join("\n");

        if self.ok {
            text
        } else if text.is_empty() {
            "ERR".to_string()
        } else {
            format!("ERR\n{text}")
        }
    }
}

struct ControlRequest {
    id: u64,
    command: String,
    reply_tx: oneshot::Sender<Result<ControlResponse, String>>,
}

impl ControlServer {
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(CONTROL_ADDR).await?;

        info!(target: "control", address = CONTROL_ADDR, "listening");

        let (request_tx, request_rx) = mpsc::channel::<ControlRequest>(CHANNEL_SIZE);

        tokio::spawn(async move {
            connection_loop(listener, request_rx).await;
        });

        Ok(Self {
            request_tx,
            next_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub async fn request(&self, command: impl Into<String>) -> Result<ControlResponse, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();

        self.request_tx
            .send(ControlRequest {
                id,
                command: sanitize_request_command(command.into()),
                reply_tx,
            })
            .await
            .map_err(|_| "control task is not running".to_string())?;

        reply_rx
            .await
            .map_err(|_| "control task dropped the request".to_string())?
    }
}

async fn connection_loop(listener: TcpListener, mut request_rx: mpsc::Receiver<ControlRequest>) {
    loop {
        info!(target: "control", "waiting for DOSBox-X");

        tokio::select! {
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(result) => result,
                    Err(error) => {
                        error!(target: "control", %error, "accept failed");
                        continue;
                    }
                };

                info!(target: "control", %peer, "DOSBox-X connected");

                if let Err(error) = handle_connection(socket, &mut request_rx).await {
                    error!(target: "control", %error, "connection error");
                }

                info!(target: "control", "DOSBox-X disconnected");
            }

            request = request_rx.recv() => {
                let Some(request) = request else {
                    return;
                };

                let _ = request
                    .reply_tx
                    .send(Err("DOSBox-X is not connected".to_string()));
            }
        }
    }
}

async fn handle_connection(
    socket: TcpStream,
    request_rx: &mut mpsc::Receiver<ControlRequest>,
) -> io::Result<()> {
    socket.set_nodelay(true)?;

    let (reader, mut writer) = socket.into_split();

    let reader = BufReader::new(reader);
    let mut lines = reader.lines();

    loop {
        tokio::select! {
            incoming = lines.next_line() => {
                match incoming? {
                    Some(line) => {
                        if !line.trim().is_empty() {
                            warn!(target: "control", line = %line, "unexpected DOSBox-X line");
                        }
                    }

                    None => {
                        return Ok(());
                    }
                }
            }

            request = request_rx.recv() => {
                let Some(request) = request else {
                    return Ok(());
                };

                process_request(request, &mut writer, &mut lines).await?;
            }
        }
    }
}

async fn process_request(
    request: ControlRequest,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
) -> io::Result<()> {
    let request_line = format!("REQ {} {}\n", request.id, request.command);

    if let Err(error) = writer.write_all(request_line.as_bytes()).await {
        let _ = request
            .reply_tx
            .send(Err(format!("failed to send request to DOSBox-X: {error}")));
        return Err(error);
    }

    if let Err(error) = writer.flush().await {
        let _ = request
            .reply_tx
            .send(Err(format!("failed to flush request to DOSBox-X: {error}")));
        return Err(error);
    }

    let result = timeout(REQUEST_TIMEOUT, read_response(request.id, lines)).await;

    match result {
        Ok(Ok(response)) => {
            let _ = request.reply_tx.send(Ok(response));
            Ok(())
        }
        Ok(Err(error)) => {
            let _ = request.reply_tx.send(Err(format!(
                "failed to read response from DOSBox-X: {error}"
            )));
            Err(error)
        }
        Err(_) => {
            let error = io::Error::new(io::ErrorKind::TimedOut, "DOSBox-X request timed out");
            let _ = request.reply_tx.send(Err(error.to_string()));
            Err(error)
        }
    }
}

async fn read_response(
    expected_id: u64,
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
) -> io::Result<ControlResponse> {
    loop {
        let Some(line) = lines.next_line().await? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before response",
            ));
        };

        if let Some((id, ok)) = parse_begin_line(&line) {
            if id == expected_id {
                return read_response_body(expected_id, ok, lines).await;
            }

            drain_response_body(id, lines).await?;
        }
    }
}

async fn read_response_body(
    expected_id: u64,
    ok: bool,
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
) -> io::Result<ControlResponse> {
    let mut body = Vec::new();

    loop {
        let Some(line) = lines.next_line().await? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed inside response",
            ));
        };

        if parse_end_line(&line) == Some(expected_id) {
            return Ok(ControlResponse { ok, lines: body });
        }

        body.push(line);
    }
}

async fn drain_response_body(
    response_id: u64,
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
) -> io::Result<()> {
    loop {
        let Some(line) = lines.next_line().await? else {
            return Ok(());
        };

        if parse_end_line(&line) == Some(response_id) {
            return Ok(());
        }
    }
}

fn parse_begin_line(line: &str) -> Option<(u64, bool)> {
    let mut parts = line.split_ascii_whitespace();

    if parts.next()? != "BEGIN" {
        return None;
    }

    let id = parts.next()?.parse().ok()?;
    let status = parts.next()?;

    match status {
        "OK" => Some((id, true)),
        "ERR" => Some((id, false)),
        _ => None,
    }
}

fn parse_end_line(line: &str) -> Option<u64> {
    let mut parts = line.split_ascii_whitespace();

    if parts.next()? != "END" {
        return None;
    }

    parts.next()?.parse().ok()
}

fn sanitize_request_command(command: String) -> String {
    command
        .chars()
        .map(|ch| if ch == '\r' || ch == '\n' { ' ' } else { ch })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_begin_line, parse_end_line, sanitize_request_command};

    #[test]
    fn parses_response_markers() {
        assert_eq!(parse_begin_line("BEGIN 42 OK"), Some((42, true)));
        assert_eq!(parse_begin_line("BEGIN 42 ERR"), Some((42, false)));
        assert_eq!(parse_end_line("END 42"), Some(42));
    }

    #[test]
    fn rejects_invalid_response_markers() {
        assert_eq!(parse_begin_line("BEGIN event OK"), None);
        assert_eq!(parse_begin_line("BEGIN 42 MAYBE"), None);
        assert_eq!(parse_end_line("END event"), None);
    }

    #[test]
    fn sanitizes_request_commands_to_single_line() {
        assert_eq!(
            sanitize_request_command("EXEC CPU\r\nPING".to_string()),
            "EXEC CPU  PING"
        );
    }
}
