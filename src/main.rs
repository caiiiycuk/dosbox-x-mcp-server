use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use rmcp::{
    ServiceExt, handler::server::wrapper::Parameters, schemars, tool, tool_router, transport::stdio,
};
use tracing_subscriber::{EnvFilter, fmt::MakeWriter};

mod control_server;
use control_server::ControlServer;

const CAPABILITIES: &str = r#"DOSBox-X debugger command catalog

Use debug_help for the live command list from the connected DOSBox-X build.
Use debug_exec for commands not wrapped by a higher-level MCP tool.
Call debug_capabilities or debug_help before using unfamiliar commands.

State:
CPU             Display CPU status information.
FPU             Display FPU status information.
PIC             Display interrupt controller state.
PAGING [page]   Display page table information.
GDT             List GDT descriptors.
LDT             List LDT descriptors.
IDT             List IDT descriptors.
SELINFO [name]  Show selector information.

DOS and memory:
DOS MCBS        Show DOS Memory Control Block chain.
DOS KERN        Show DOS kernel memory blocks.
DOS XMS         Show XMS memory handles.
DOS EMS         Show EMS memory handles.
DOS DEVS        Show DOS device list.
BIOS MEM        Show BIOS memory blocks.
EMU MEM         Show emulator memory information.
EMU MACHINE     Show emulator machine information.

Control:
RUN             Continue execution.
RUNWATCH        Continue execution and break on watched state.
VRT             Continue until next vertical retrace.
BP args         Set code breakpoint.
BPINT args      Set interrupt breakpoint.
BPLIST          List breakpoints.
BPDEL args      Delete breakpoint number or *.

Heavy debug, requires C_HEAVY_DEBUG in DOSBox-X:
BPM args        Set segmented memory-change breakpoint.
BPPM args       Set protected-mode memory-change breakpoint.
BPLM args       Set linear memory-change breakpoint.
LOG args        Write CPU log file.
LOGS args       Write short CPU log file.
LOGL args       Write long CPU log file.
LOGC args       Write CS:IP-only CPU log file.
HEAVYLOG        Toggle automatic CPU log on exit.
ZEROPROTECT     Toggle zero code execution detection.

Search and evaluate:
MEMFIND args    Start or inspect memory search instance.
MEMS args       Continue memory search instance.
EV args         Evaluate register/value expressions.
D args          Set segmented data view address.
DV args         Set virtual data view address.
DP args         Set physical data view address.
"#;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExecParams {
    command: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RunParams {
    mode: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct BreakpointParams {
    action: String,
    args: Option<String>,
}

#[derive(Clone)]
struct DosboxDebugServer {
    control: ControlServer,
}

#[tool_router(server_handler)]
impl DosboxDebugServer {
    #[tool(description = "Check whether DOSBox-X is connected to the debugger control server.")]
    async fn dosbox_ping(&self) -> String {
        self.control_request("PING").await
    }

    #[tool(description = "Ask DOSBox-X to enter the built-in debugger.")]
    async fn debug_break(&self) -> String {
        self.control_request("BREAK").await
    }

    #[tool(
        description = "Execute one raw DOSBox-X debugger command. Call debug_capabilities or debug_help before using unfamiliar commands."
    )]
    async fn debug_exec(
        &self,
        Parameters(ExecParams { command }): Parameters<ExecParams>,
    ) -> String {
        self.exec_debugger_command(command).await
    }

    #[tool(description = "Return the live HELP output from the connected DOSBox-X debugger build.")]
    async fn debug_help(&self) -> String {
        self.exec_debugger_command("HELP").await
    }

    #[tool(
        description = "Return the static catalog of known DOSBox-X debugger commands and wrappers."
    )]
    fn debug_capabilities(&self) -> String {
        CAPABILITIES.to_string()
    }

    #[tool(
        description = "Collect a compact debugger snapshot: CPU, PIC, PAGING, EMU MEM, and EMU MACHINE."
    )]
    async fn debug_snapshot(&self) -> String {
        let mut sections = Vec::new();

        for command in ["CPU", "PIC", "PAGING", "EMU MEM", "EMU MACHINE"] {
            let output = self.exec_debugger_command(command).await;
            sections.push(format!("== {command} ==\n{output}"));
        }

        sections.join("\n\n")
    }

    #[tool(description = "Continue execution with mode run, runwatch, or vrt.")]
    async fn debug_run(&self, Parameters(RunParams { mode }): Parameters<RunParams>) -> String {
        let command = match mode.to_ascii_lowercase().as_str() {
            "run" | "normal" => "RUN",
            "runwatch" | "watch" => "RUNWATCH",
            "vrt" | "retrace" => "VRT",
            _ => return "ERR\nmode must be one of: run, runwatch, vrt".to_string(),
        };

        self.exec_debugger_command(command).await
    }

    #[tool(
        description = "Manage debugger breakpoints. Actions: set/code/bp, int/bpint, mem/bpm, pmem/bppm, lmem/bplm, delete/bpdel, list/bplist."
    )]
    async fn debug_breakpoint(
        &self,
        Parameters(BreakpointParams { action, args }): Parameters<BreakpointParams>,
    ) -> String {
        let args = args.unwrap_or_default();
        let command = match action.to_ascii_lowercase().as_str() {
            "set" | "code" | "bp" => format_command("BP", &args),
            "int" | "bpint" => format_command("BPINT", &args),
            "mem" | "bpm" => format_command("BPM", &args),
            "pmem" | "bppm" => format_command("BPPM", &args),
            "lmem" | "bplm" => format_command("BPLM", &args),
            "delete" | "del" | "bpdel" => format_command("BPDEL", &args),
            "list" | "bplist" => "BPLIST".to_string(),
            _ => {
                return "ERR\naction must be one of: set, int, mem, pmem, lmem, delete, list"
                    .to_string();
            }
        };

        self.exec_debugger_command(command).await
    }
}

impl DosboxDebugServer {
    async fn exec_debugger_command(&self, command: impl AsRef<str>) -> String {
        self.control_request(format!("EXEC {}", command.as_ref().trim()))
            .await
    }

    async fn control_request(&self, command: impl Into<String>) -> String {
        match self.control.request(command).await {
            Ok(response) => response.into_text(),
            Err(error) => format!("ERR\n{error}"),
        }
    }
}

fn format_command(command: &str, args: &str) -> String {
    let args = args.trim();

    if args.is_empty() {
        command.to_string()
    } else {
        format!("{command} {args}")
    }
}

#[derive(Clone)]
struct TeeMakeWriter {
    log_file: Arc<Mutex<File>>,
}

struct TeeWriter {
    log_file: Arc<Mutex<File>>,
    stderr: io::Stderr,
}

impl<'a> MakeWriter<'a> for TeeMakeWriter {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter {
            log_file: Arc::clone(&self.log_file),
            stderr: io::stderr(),
        }
    }
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stderr.write_all(buf)?;

        let mut log_file = self
            .log_file
            .lock()
            .map_err(|_| io::Error::other("log file lock poisoned"))?;
        log_file.write_all(buf)?;

        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stderr.flush()?;

        let mut log_file = self
            .log_file
            .lock()
            .map_err(|_| io::Error::other("log file lock poisoned"))?;
        log_file.flush()
    }
}

fn init_logging() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("warn,control=info,dosbox_x_mcp_server=info")),
        )
        .with_writer(TeeMakeWriter {
            log_file: Arc::new(Mutex::new(open_log_file()?)),
        })
        .init();

    Ok(())
}

fn open_log_file() -> anyhow::Result<File> {
    let log_dir = home_dir()?.join(".dosbox-x-mcp-server");
    fs::create_dir_all(&log_dir)?;

    Ok(OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("server.log"))?)
}

fn home_dir() -> anyhow::Result<PathBuf> {
    if let Some(home) = env::var_os("HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }

    if let Some(user_profile) = env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(user_profile));
    }

    if let (Some(home_drive), Some(home_path)) = (env::var_os("HOMEDRIVE"), env::var_os("HOMEPATH"))
    {
        let mut path = PathBuf::from(home_drive);
        path.push(home_path);
        return Ok(path);
    }

    Err(anyhow::anyhow!("home directory environment is not set"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging()?;

    let control = ControlServer::start().await?;
    let service = DosboxDebugServer { control }.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
