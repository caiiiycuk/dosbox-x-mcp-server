use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{TcpListener, TcpStream, tcp::OwnedReadHalf, tcp::OwnedWriteHalf},
    sync::{mpsc, oneshot},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, accept_async,
    tungstenite::{self, Message},
};
use tracing::{error, info, warn};

pub const DEFAULT_CONTROL_PORT: u16 = 58991;
pub const DEFAULT_WEBSOCKET_PORT: u16 = 58992;

const CHANNEL_SIZE: usize = 1024;
const MAX_RECEIVE_BUFFER: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct ControlServer {
    request_tx: mpsc::Sender<ControlRequest>,
    next_id: Arc<AtomicU64>,
    address: SocketAddr,
    websocket_address: SocketAddr,
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

enum ControlConnection {
    Tcp {
        peer: SocketAddr,
        lines: Lines<BufReader<OwnedReadHalf>>,
        writer: OwnedWriteHalf,
    },
    WebSocket {
        peer: SocketAddr,
        socket: Box<WebSocketStream<TcpStream>>,
        receive_buffer: String,
    },
}

impl ControlConnection {
    fn tcp(socket: TcpStream, peer: SocketAddr) -> io::Result<Self> {
        socket.set_nodelay(true)?;
        let (reader, writer) = socket.into_split();

        Ok(Self::Tcp {
            peer,
            lines: BufReader::new(reader).lines(),
            writer,
        })
    }

    fn websocket(socket: WebSocketStream<TcpStream>, peer: SocketAddr) -> Self {
        Self::WebSocket {
            peer,
            socket: Box::new(socket),
            receive_buffer: String::new(),
        }
    }

    fn peer(&self) -> SocketAddr {
        match self {
            Self::Tcp { peer, .. } | Self::WebSocket { peer, .. } => *peer,
        }
    }

    fn transport(&self) -> &'static str {
        match self {
            Self::Tcp { .. } => "tcp",
            Self::WebSocket { .. } => "websocket",
        }
    }

    async fn send(&mut self, message: String) -> io::Result<()> {
        match self {
            Self::Tcp { writer, .. } => {
                writer.write_all(message.as_bytes()).await?;
                writer.flush().await
            }
            Self::WebSocket { socket, .. } => socket
                .send(Message::Text(message.into()))
                .await
                .map_err(websocket_error),
        }
    }

    async fn next_line(&mut self) -> io::Result<Option<String>> {
        match self {
            Self::Tcp { lines, .. } => lines.next_line().await,
            Self::WebSocket {
                socket,
                receive_buffer,
                ..
            } => loop {
                if let Some(line) = take_line(receive_buffer)? {
                    return Ok(Some(line));
                }

                let Some(message) = socket.next().await else {
                    return Ok(None);
                };

                match message.map_err(websocket_error)? {
                    Message::Text(text) => {
                        receive_buffer.push_str(text.as_ref());
                    }
                    Message::Ping(data) => {
                        socket
                            .send(Message::Pong(data))
                            .await
                            .map_err(websocket_error)?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => return Ok(None),
                    Message::Binary(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "binary WebSocket messages are not supported",
                        ));
                    }
                    Message::Frame(_) => {}
                }
            },
        }
    }
}

impl ControlServer {
    pub async fn start() -> io::Result<Self> {
        let default_address = SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_CONTROL_PORT));
        let listener = bind_control_listener(default_address, "tcp").await?;
        let address = listener.local_addr()?;

        let default_websocket_address =
            SocketAddr::from((Ipv4Addr::LOCALHOST, DEFAULT_WEBSOCKET_PORT));
        let websocket_listener =
            bind_control_listener(default_websocket_address, "websocket").await?;
        let websocket_address = websocket_listener.local_addr()?;

        info!(target: "control", %address, transport = "tcp", "listening");
        info!(target: "control", address = %websocket_address, transport = "websocket", "listening");

        let (request_tx, request_rx) = mpsc::channel::<ControlRequest>(CHANNEL_SIZE);

        tokio::spawn(async move {
            connection_loop(listener, websocket_listener, request_rx).await;
        });

        Ok(Self {
            request_tx,
            next_id: Arc::new(AtomicU64::new(1)),
            address,
            websocket_address,
        })
    }

    pub fn port(&self) -> u16 {
        self.address.port()
    }

    pub fn websocket_port(&self) -> u16 {
        self.websocket_address.port()
    }

    pub async fn request(&self, command: impl Into<String>) -> Result<ControlResponse, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        let command = sanitize_request_command(command.into());

        info!(target: "control", request_id = id, command = %command, "DOSBox-X request queued");

        self.request_tx
            .send(ControlRequest {
                id,
                command,
                reply_tx,
            })
            .await
            .map_err(|_| "control task is not running".to_string())?;

        reply_rx
            .await
            .map_err(|_| "control task dropped the request".to_string())?
    }
}

async fn bind_control_listener(
    default_address: SocketAddr,
    transport: &'static str,
) -> io::Result<TcpListener> {
    match TcpListener::bind(default_address).await {
        Ok(listener) => Ok(listener),
        Err(error) => {
            warn!(
                target: "control",
                address = %default_address,
                transport,
                %error,
                "default control port is unavailable; choosing another port"
            );

            TcpListener::bind(SocketAddr::new(default_address.ip(), 0)).await
        }
    }
}

async fn connection_loop(
    listener: TcpListener,
    websocket_listener: TcpListener,
    mut request_rx: mpsc::Receiver<ControlRequest>,
) {
    loop {
        info!(target: "control", "waiting for DOSBox-X");

        tokio::select! {
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(result) => result,
                    Err(error) => {
                        error!(target: "control", transport = "tcp", %error, "accept failed");
                        continue;
                    }
                };

                match ControlConnection::tcp(socket, peer) {
                    Ok(connection) => handle_connected_client(connection, &mut request_rx).await,
                    Err(error) => error!(target: "control", transport = "tcp", %peer, %error, "connection setup failed"),
                }
            }

            accepted = websocket_listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(result) => result,
                    Err(error) => {
                        error!(target: "control", transport = "websocket", %error, "accept failed");
                        continue;
                    }
                };

                if let Err(error) = socket.set_nodelay(true) {
                    error!(target: "control", transport = "websocket", %peer, %error, "connection setup failed");
                    continue;
                }

                match timeout(REQUEST_TIMEOUT, accept_async(socket)).await {
                    Ok(Ok(socket)) => {
                        let connection = ControlConnection::websocket(socket, peer);
                        handle_connected_client(connection, &mut request_rx).await;
                    }
                    Ok(Err(error)) => error!(target: "control", transport = "websocket", %peer, %error, "WebSocket handshake failed"),
                    Err(_) => warn!(target: "control", transport = "websocket", %peer, "WebSocket handshake timed out"),
                }
            }

            request = request_rx.recv() => {
                let Some(request) = request else {
                    return;
                };

                reject_disconnected_request(request);
            }
        }
    }
}

async fn handle_connected_client(
    mut connection: ControlConnection,
    request_rx: &mut mpsc::Receiver<ControlRequest>,
) {
    let peer = connection.peer();
    let transport = connection.transport();
    info!(target: "control", %peer, transport, "DOSBox-X connected");

    if let Err(error) = handle_connection(&mut connection, request_rx).await {
        error!(target: "control", %peer, transport, %error, "connection error");
    }

    info!(target: "control", %peer, transport, "DOSBox-X disconnected");
}

fn reject_disconnected_request(request: ControlRequest) {
    warn!(target: "control", request_id = request.id, command = %request.command, "DOSBox-X request rejected: not connected");

    let _ = request
        .reply_tx
        .send(Err("DOSBox-X is not connected".to_string()));
}

async fn handle_connection(
    connection: &mut ControlConnection,
    request_rx: &mut mpsc::Receiver<ControlRequest>,
) -> io::Result<()> {
    loop {
        tokio::select! {
            incoming = connection.next_line() => {
                match incoming? {
                    Some(line) => {
                        if !line.trim().is_empty() {
                            warn!(target: "control", line = %line, "unexpected DOSBox-X line");
                        }
                    }

                    None => return Ok(()),
                }
            }

            request = request_rx.recv() => {
                let Some(request) = request else {
                    return Ok(());
                };

                process_request(request, connection).await?;
            }
        }
    }
}

async fn process_request(
    request: ControlRequest,
    connection: &mut ControlConnection,
) -> io::Result<()> {
    let request_line = format!("REQ {} {}\n", request.id, request.command);

    info!(target: "control", request_id = request.id, command = %request.command, "DOSBox-X request send begin");

    if let Err(error) = connection.send(request_line).await {
        error!(target: "control", request_id = request.id, command = %request.command, %error, "DOSBox-X request send failed");

        let _ = request
            .reply_tx
            .send(Err(format!("failed to send request to DOSBox-X: {error}")));
        return Err(error);
    }

    info!(target: "control", request_id = request.id, command = %request.command, "DOSBox-X request sent; waiting for response");

    let result = timeout(REQUEST_TIMEOUT, read_response(request.id, connection)).await;

    match result {
        Ok(Ok(response)) => {
            info!(
                target: "control",
                request_id = request.id,
                command = %request.command,
                ok = response.ok,
                line_count = response.lines.len(),
                "DOSBox-X response received"
            );

            let _ = request.reply_tx.send(Ok(response));
            Ok(())
        }
        Ok(Err(error)) => {
            error!(target: "control", request_id = request.id, command = %request.command, %error, "DOSBox-X response read failed");

            let _ = request.reply_tx.send(Err(format!(
                "failed to read response from DOSBox-X: {error}"
            )));
            Err(error)
        }
        Err(_) => {
            let error = io::Error::new(io::ErrorKind::TimedOut, "DOSBox-X request timed out");
            error!(target: "control", request_id = request.id, command = %request.command, %error, "DOSBox-X response timed out");

            let _ = request.reply_tx.send(Err(error.to_string()));
            Err(error)
        }
    }
}

async fn read_response(
    expected_id: u64,
    connection: &mut ControlConnection,
) -> io::Result<ControlResponse> {
    loop {
        let Some(line) = connection.next_line().await? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before response",
            ));
        };

        if let Some((id, ok)) = parse_begin_line(&line) {
            if id == expected_id {
                return read_response_body(expected_id, ok, connection).await;
            }

            drain_response_body(id, connection).await?;
        }
    }
}

async fn read_response_body(
    expected_id: u64,
    ok: bool,
    connection: &mut ControlConnection,
) -> io::Result<ControlResponse> {
    let mut body = Vec::new();

    loop {
        let Some(line) = connection.next_line().await? else {
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
    connection: &mut ControlConnection,
) -> io::Result<()> {
    loop {
        let Some(line) = connection.next_line().await? else {
            return Ok(());
        };

        if parse_end_line(&line) == Some(response_id) {
            return Ok(());
        }
    }
}

fn take_line(buffer: &mut String) -> io::Result<Option<String>> {
    let Some(newline) = buffer.find('\n') else {
        if buffer.len() > MAX_RECEIVE_BUFFER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WebSocket receive buffer overflow",
            ));
        }

        return Ok(None);
    };

    if newline > MAX_RECEIVE_BUFFER {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "WebSocket receive buffer overflow",
        ));
    }

    let mut line = buffer[..newline].to_string();
    buffer.drain(..=newline);

    if line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

fn websocket_error(error: tungstenite::Error) -> io::Error {
    io::Error::other(error)
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
    use futures_util::{SinkExt, StreamExt};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::{TcpListener, TcpStream},
        time::{Duration, timeout},
    };
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    use super::{
        ControlServer, bind_control_listener, parse_begin_line, parse_end_line,
        sanitize_request_command,
    };

    #[tokio::test]
    async fn uses_another_port_when_default_is_unavailable() {
        let occupied_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let occupied_address = occupied_listener.local_addr().unwrap();

        let listener = bind_control_listener(occupied_address, "test")
            .await
            .unwrap();

        assert_ne!(
            listener.local_addr().unwrap().port(),
            occupied_address.port()
        );
    }

    #[tokio::test]
    async fn handles_tcp_request() {
        let server = ControlServer::start().await.unwrap();
        let port = server.port();

        let client = tokio::spawn(async move {
            let socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();

            let request = lines.next_line().await.unwrap().unwrap();
            let id = request.split_ascii_whitespace().nth(1).unwrap();
            assert!(request.ends_with("PING"));

            writer
                .write_all(format!("BEGIN {id} OK\nPONG\nEND {id}\n").as_bytes())
                .await
                .unwrap();
        });

        let response = timeout(Duration::from_secs(1), async {
            loop {
                match server.request("PING").await {
                    Err(error) if error == "DOSBox-X is not connected" => {
                        tokio::task::yield_now().await;
                    }
                    result => break result,
                }
            }
        })
        .await
        .unwrap()
        .unwrap();

        assert!(response.ok);
        assert_eq!(response.lines, ["PONG"]);
        client.await.unwrap();
    }

    #[tokio::test]
    async fn handles_websocket_request() {
        let server = ControlServer::start().await.unwrap();
        let url = format!("ws://127.0.0.1:{}", server.websocket_port());
        let (mut socket, _) = connect_async(url).await.unwrap();

        let request_task = tokio::spawn({
            let server = server.clone();
            async move { server.request("EXEC CPU").await }
        });

        let request = timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let id = request.split_ascii_whitespace().nth(1).unwrap();
        assert!(request.ends_with("EXEC CPU\n"));

        socket
            .send(Message::Text(
                format!("BEGIN {id} OK\nline one\nline two\nEND {id}\n").into(),
            ))
            .await
            .unwrap();

        let response = timeout(Duration::from_secs(1), request_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert!(response.ok);
        assert_eq!(response.lines, ["line one", "line two"]);
    }

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
