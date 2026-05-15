use zed_extension_api::{
    self as zed, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, DebugTaskDefinition,
    LanguageServerId, Result, StartDebuggingRequestArguments,
    StartDebuggingRequestArgumentsRequest, resolve_tcp_template, settings::LspSettings,
};

use std::{
    collections::BTreeSet,
    net::Ipv4Addr,
    path::{Component, Path, PathBuf},
};

const SERVER_NAME: &str = "miden-lsp";
const ADAPTER_NAME: &str = "miden";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 4711;
const DEFAULT_TCP_TIMEOUT_MS: u64 = 15_000;

const DAP_TCP_PROXY: &str = r#"
import json
import socket
import subprocess
import sys
import threading
import time


def log(message):
    print("[miden-zed-proxy] " + message, file=sys.stderr, flush=True)


def fail(message):
    log(message)
    sys.exit(1)


def take_value(args, flag, default=None):
    if flag not in args:
        return default
    index = args.index(flag)
    if index + 1 >= len(args):
        fail("missing value for " + flag)
    value = args[index + 1]
    del args[index:index + 2]
    return value


def source_name(path):
    if not path:
        return None
    return path.rsplit("/", 1)[-1] or path


def forward_pipe(pipe):
    try:
        while True:
            line = pipe.readline()
            if not line:
                return
            sys.stderr.buffer.write(line)
            sys.stderr.buffer.flush()
    except Exception:
        return


def connect_to_adapter(host, port, timeout_ms, process):
    deadline = time.monotonic() + (timeout_ms / 1000.0)
    last_error = None
    while time.monotonic() < deadline:
        if process is not None and process.poll() is not None:
            fail("debug adapter process exited before the proxy connected")
        try:
            sock = socket.create_connection((host, port), timeout=0.5)
            sock.settimeout(None)
            log("connected to Miden DAP at %s:%s" % (host, port))
            return sock
        except OSError as error:
            last_error = error
            time.sleep(0.1)
    fail("timed out connecting to Miden DAP at %s:%s: %s" % (host, port, last_error))


def describe_message(body):
    try:
        message = json.loads(body.decode("utf-8"))
    except Exception as error:
        return "unparseable DAP message: %s" % error

    if not isinstance(message, dict):
        return "non-object DAP message"

    message_type = message.get("type")
    if message_type == "request":
        return "request:%s seq:%s" % (message.get("command"), message.get("seq"))
    if message_type == "response":
        return "response:%s seq:%s request_seq:%s success:%s" % (
            message.get("command"),
            message.get("seq"),
            message.get("request_seq"),
            message.get("success"),
        )
    if message_type == "event":
        return "event:%s seq:%s" % (message.get("event"), message.get("seq"))
    return "%s seq:%s" % (message_type, message.get("seq"))


def remember_client_message(body, state):
    try:
        message = json.loads(body.decode("utf-8"))
    except Exception:
        return

    if not isinstance(message, dict):
        return
    if message.get("type") != "request" or message.get("command") != "setBreakpoints":
        return

    arguments = message.get("arguments")
    if not isinstance(arguments, dict):
        return
    source = arguments.get("source")
    if not isinstance(source, dict):
        return

    path = source.get("path")
    if isinstance(path, str) and path:
        state["source_path"] = path
        state["source_name"] = source.get("name") or source_name(path)
        log("remembered source path for stack traces: " + path)


def read_frame(reader):
    header = bytearray()
    while not header.endswith(b"\r\n\r\n"):
        chunk = reader.read(1)
        if not chunk:
            return None
        header.extend(chunk)

    content_length = None
    for line in header.decode("ascii", "replace").split("\r\n"):
        if line.lower().startswith("content-length:"):
            content_length = int(line.split(":", 1)[1].strip())
            break
    if content_length is None:
        raise RuntimeError("missing DAP Content-Length header")

    body = reader.read(content_length)
    if len(body) != content_length:
        return None
    return body


def normalize_stack_frame(frame, state):
    if not isinstance(frame, dict):
        return False

    changed = False
    source = frame.get("source")
    if not isinstance(source, dict) and state.get("source_path") and frame.get("line", 0) == 0:
        frame["source"] = {
            "name": state.get("source_name") or source_name(state.get("source_path")),
            "path": state["source_path"],
        }
        frame["line"] = 1
        changed = True

    if isinstance(frame.get("source"), dict) and frame.get("column", 0) == 0:
        frame["column"] = 1
        changed = True

    return changed


def normalize_server_message(body, state):
    try:
        message = json.loads(body.decode("utf-8"))
    except Exception:
        return body

    changed = False
    if (
        isinstance(message, dict)
        and message.get("type") == "event"
        and message.get("event") == "initialized"
        and "body" not in message
    ):
        log("normalizing initialized event with missing body")
        message["body"] = {}
        changed = True

    if isinstance(message, dict) and message.get("type") == "event" and message.get("event") == "miden/uiState":
        event_body = message.get("body")
        if isinstance(event_body, dict):
            for frame in event_body.get("callstack") or []:
                source_path = frame.get("source_path") if isinstance(frame, dict) else None
                if isinstance(source_path, str) and source_path:
                    state["source_path"] = source_path
                    state["source_name"] = source_name(source_path)
                    break
        log("dropping miden/uiState event; Zed only consumes standard DAP messages")
        return None

    if isinstance(message, dict) and message.get("type") == "response" and message.get("command") == "stackTrace":
        response_body = message.get("body")
        if isinstance(response_body, dict):
            for frame in response_body.get("stackFrames") or []:
                changed = normalize_stack_frame(frame, state) or changed
            if changed:
                log("normalized stackTrace frame source/column for Zed")

    if changed:
        return json.dumps(message, separators=(",", ":")).encode("utf-8")

    return body


def write_frame_to_socket(sock, body):
    sock.sendall(b"Content-Length: %d\r\n\r\n" % len(body))
    sock.sendall(body)


def write_frame_to_stdout(body):
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()


def copy_client_to_server(sock, state):
    try:
        while True:
            body = read_frame(sys.stdin.buffer)
            if body is None:
                log("stdin closed; closing client-to-server stream")
                try:
                    sock.shutdown(socket.SHUT_WR)
                except OSError:
                    pass
                return
            log("client -> server " + describe_message(body))
            remember_client_message(body, state)
            write_frame_to_socket(sock, body)
    except Exception as error:
        log("client-to-server proxy stopped: %s" % error)


def copy_server_to_client(sock, state):
    reader = sock.makefile("rb", buffering=0)
    try:
        while True:
            body = read_frame(reader)
            if body is None:
                log("Miden DAP closed server-to-client stream")
                return
            log("server -> client " + describe_message(body))
            body = normalize_server_message(body, state)
            if body is None:
                continue
            write_frame_to_stdout(body)
    except Exception as error:
        log("server-to-client proxy stopped: %s" % error)
        raise


def main():
    args = sys.argv[1:]
    adapter_argv = None
    if "--adapter-argv" in args:
        index = args.index("--adapter-argv")
        adapter_argv = args[index + 1:]
        args = args[:index]

    host = take_value(args, "--host", "127.0.0.1")
    port = int(take_value(args, "--port", "4711"))
    timeout_ms = int(take_value(args, "--timeout-ms", "15000"))
    adapter_cwd = take_value(args, "--adapter-cwd", None)
    source_path_for_ui = take_value(args, "--source-path-for-ui", None)
    if args:
        fail("unknown proxy arguments: " + repr(args))

    state = {
        "source_path": source_path_for_ui,
        "source_name": source_name(source_path_for_ui),
    }

    process = None
    sock = None
    try:
        if adapter_argv:
            log("starting debug adapter process: " + repr(adapter_argv))
            process = subprocess.Popen(
                adapter_argv,
                cwd=adapter_cwd or None,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            threading.Thread(target=forward_pipe, args=(process.stdout,), daemon=True).start()
            threading.Thread(target=forward_pipe, args=(process.stderr,), daemon=True).start()
        else:
            log("attaching to existing debug adapter")

        sock = connect_to_adapter(host, port, timeout_ms, process)
        threading.Thread(target=copy_client_to_server, args=(sock, state), daemon=True).start()
        copy_server_to_client(sock, state)
    finally:
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()


if __name__ == "__main__":
    main()
"#;

struct MidenExtension {
    cached_binary_path: Option<String>,
}

impl MidenExtension {
    fn command_from_path(
        &mut self,
        path: String,
        args: Vec<String>,
        env: zed::EnvVars,
    ) -> zed::Command {
        self.cached_binary_path = Some(path.clone());
        zed::Command {
            command: path,
            args,
            env,
        }
    }
}

impl zed::Extension for MidenExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        if language_server_id.as_ref() != SERVER_NAME {
            return Err(format!(
                "unknown language server ID {}",
                language_server_id.as_ref()
            ));
        }

        let binary_settings = LspSettings::for_worktree(SERVER_NAME, worktree)
            .ok()
            .and_then(|lsp_settings| lsp_settings.binary);
        let args = binary_settings
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();
        let env = worktree.shell_env();

        if let Some(path) = binary_settings.and_then(|binary| binary.path) {
            return Ok(self.command_from_path(path, args, env));
        }

        if let Some(path) = self.cached_binary_path.clone() {
            return Ok(zed::Command {
                command: path,
                args,
                env,
            });
        }

        if let Some(path) = worktree.which(SERVER_NAME) {
            return Ok(self.command_from_path(path, args, env));
        }

        Err(
            "miden-lsp was not found in PATH; configure lsp.binary.path or install miden-lsp"
                .to_string(),
        )
    }

    fn language_server_workspace_configuration(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        LspSettings::for_worktree(server_id.as_ref(), worktree)
            .map(|lsp_settings| lsp_settings.settings)
    }

    fn language_server_initialization_options(
        &mut self,
        server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        LspSettings::for_worktree(server_id.as_ref(), worktree)
            .map(|lsp_settings| lsp_settings.initialization_options)
    }

    // -------------------------------------------------------------------------
    // Debug Adapter Protocol
    //
    // The Miden DAP server is exposed over TCP. Zed talks stdio to a Python
    // proxy so we can keep Zed's DAP transport stable while adapting server
    // quirks at the boundary.
    // Two flows:
    //   - launch: the proxy spawns `miden-client` for transaction scripts or
    //     `miden-debug` for standalone programs, then connects to its listener.
    //   - attach: the user starts the server; the proxy connects to it.
    //
    // The proxy normalizes transport/UI compatibility details only. It leaves
    // DAP scopes and variables unchanged so Zed sees the `Local Variables`,
    // `Operand Stack`, and `Memory` scopes emitted by miden-debug.
    // -------------------------------------------------------------------------

    fn dap_request_kind(
        &mut self,
        adapter_name: String,
        config: zed::serde_json::Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest> {
        if adapter_name != ADAPTER_NAME {
            return Err(format!("unknown debug adapter ID {adapter_name}"));
        }
        let config = debug_config_value(&config);
        match config
            .get("request")
            .and_then(zed::serde_json::Value::as_str)
        {
            Some("attach") => Ok(StartDebuggingRequestArgumentsRequest::Attach),
            Some("launch") => Ok(StartDebuggingRequestArgumentsRequest::Launch),
            Some(other) => Err(format!("unknown `request` kind: {other}")),
            // Heuristic when `request` is omitted: a configured program path
            // implies launch, otherwise attach to a server the user started.
            None if config.get("scriptPath").is_some() || config.get("programPath").is_some() => {
                Ok(StartDebuggingRequestArgumentsRequest::Launch)
            }
            None => Ok(StartDebuggingRequestArgumentsRequest::Attach),
        }
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<DebugAdapterBinary> {
        if adapter_name != ADAPTER_NAME {
            return Err(format!("unknown debug adapter ID {adapter_name}"));
        }

        let raw: zed::serde_json::Value = zed::serde_json::from_str(&config.config)
            .map_err(|err| format!("invalid Miden debug config JSON: {err}"))?;
        let raw_config = debug_config_value(&raw);
        let parsed = MidenDebugConfig::from_json(raw_config);

        let (host_str, port, timeout_ms) = if let Some(tcp_template) = config.tcp_connection {
            let tcp = resolve_tcp_template(tcp_template)
                .map_err(|err| format!("failed to resolve TCP arguments: {err}"))?;
            (
                Ipv4Addr::from(tcp.host).to_string(),
                tcp.port,
                tcp.timeout.unwrap_or(DEFAULT_TCP_TIMEOUT_MS),
            )
        } else {
            let host_str = parsed
                .host
                .clone()
                .unwrap_or_else(|| DEFAULT_HOST.to_string());
            let port = parsed.port.unwrap_or(DEFAULT_PORT);
            parse_ipv4(&host_str)?;
            (host_str, port, DEFAULT_TCP_TIMEOUT_MS)
        };

        let request_kind = self.dap_request_kind(ADAPTER_NAME.to_string(), raw_config.clone())?;

        let proxy_python = parsed
            .python_path
            .clone()
            .or_else(|| worktree.which("python3"))
            .unwrap_or_else(|| "python3".to_string());

        let worktree_root = worktree.root_path();
        let proxy_cwd = Some(resolve_config_path(
            parsed.cwd.as_deref().unwrap_or("."),
            &worktree_root,
        ));

        let mut ui_source_path = parsed
            .program_path
            .clone()
            .or_else(|| parsed.script_path.clone());
        let adapter_argv = match request_kind {
            StartDebuggingRequestArgumentsRequest::Launch => {
                let runtime = parsed.runtime.as_deref().unwrap_or_else(|| {
                    if parsed.program_path.is_some() {
                        "debugger"
                    } else {
                        "client"
                    }
                });

                match runtime {
                    "client" | "miden-client" => {
                        let script_path = parsed.script_path.clone().ok_or_else(|| {
                            "`scriptPath` is required for client launch".to_string()
                        })?;
                        let script_path =
                            resolve_config_path(&script_path, proxy_cwd.as_deref().unwrap());
                        ui_source_path = Some(script_path.clone());
                        let miden_client = user_provided_debug_adapter_path
                            .or_else(|| parsed.miden_client_path.clone())
                            .or_else(|| worktree.which("miden-client"))
                            .unwrap_or_else(|| "miden-client".to_string());
                        let miden_client =
                            resolve_command_path(&miden_client, proxy_cwd.as_deref().unwrap());

                        let mut args = vec![
                            miden_client,
                            "exec".to_string(),
                            "--script-path".to_string(),
                            script_path,
                            "--start-debug-adapter".to_string(),
                            format!("{host_str}:{port}"),
                        ];
                        if let Some(account_id) = parsed.account_id.clone() {
                            args.push("--account".to_string());
                            args.push(account_id);
                        }

                        Some(args)
                    }
                    "debugger" | "miden-debug" => {
                        let program_path = parsed
                            .program_path
                            .clone()
                            .or_else(|| parsed.script_path.clone())
                            .ok_or_else(|| {
                                "`programPath` is required for debugger launch".to_string()
                            })?;
                        let program_path =
                            resolve_config_path(&program_path, proxy_cwd.as_deref().unwrap());
                        ui_source_path = Some(program_path.clone());
                        let miden_debug = user_provided_debug_adapter_path
                            .or_else(|| parsed.miden_debug_path.clone())
                            .or_else(|| worktree.which("miden-debug"))
                            .unwrap_or_else(|| "miden-debug".to_string());
                        let miden_debug =
                            resolve_command_path(&miden_debug, proxy_cwd.as_deref().unwrap());

                        let mut args = vec![
                            miden_debug,
                            "--start-debug-adapter".to_string(),
                            format!("{host_str}:{port}"),
                        ];
                        if let Some(cwd) = proxy_cwd.as_deref() {
                            args.push("--working-dir".to_string());
                            args.push(cwd.to_string());
                        }
                        push_optional_path_arg(
                            &mut args,
                            "--inputs",
                            parsed.inputs_path.clone(),
                            proxy_cwd.as_deref().unwrap(),
                        );
                        push_optional_arg(&mut args, "--entrypoint", parsed.entrypoint.clone());
                        push_optional_path_arg(
                            &mut args,
                            "--sysroot",
                            parsed.sysroot.clone(),
                            proxy_cwd.as_deref().unwrap(),
                        );
                        for search_path in &parsed.search_path {
                            args.push("--search-path".to_string());
                            args.push(resolve_config_path(
                                search_path,
                                proxy_cwd.as_deref().unwrap(),
                            ));
                        }
                        for link_library in &parsed.link_libraries {
                            args.push("--link-library".to_string());
                            args.push(link_library.clone());
                        }
                        for prefix in
                            parsed.source_path_prefixes(&program_path, proxy_cwd.as_deref())
                        {
                            args.push("--source-path-prefix".to_string());
                            args.push(prefix);
                        }

                        args.push(program_path);
                        if !parsed.program_args.is_empty() {
                            args.push("--".to_string());
                            args.extend(parsed.program_args.clone());
                        }

                        Some(args)
                    }
                    other => return Err(format!("unknown Miden launch runtime `{other}`")),
                }
            }
            StartDebuggingRequestArgumentsRequest::Attach => None,
        };

        let arguments = proxy_arguments(
            &host_str,
            port,
            timeout_ms,
            proxy_cwd.as_deref(),
            ui_source_path.as_deref(),
            adapter_argv,
        );

        Ok(DebugAdapterBinary {
            command: Some(proxy_python),
            arguments,
            envs: worktree.shell_env(),
            cwd: proxy_cwd,
            connection: None,
            request_args: StartDebuggingRequestArguments {
                configuration: config.config,
                request: request_kind,
            },
        })
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario> {
        // Translate the modal "New Debug Session" UI into our adapter-specific
        // JSON config. For attach, the modal only knows process_id, which we
        // don't use — fall back to defaults; the user can refine via debug.json.
        let json_config = match &config.request {
            DebugRequest::Launch(launch) => zed::serde_json::json!({
                "request": "launch",
                "runtime": "debugger",
                "programPath": launch.program,
                "cwd": launch.cwd,
                "host": DEFAULT_HOST,
                "port": DEFAULT_PORT,
            }),
            DebugRequest::Attach(_) => zed::serde_json::json!({
                "request": "attach",
                "host": DEFAULT_HOST,
                "port": DEFAULT_PORT,
            }),
        };

        Ok(DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            config: json_config.to_string(),
            tcp_connection: None,
        })
    }
}

#[derive(Default)]
struct MidenDebugConfig {
    runtime: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    script_path: Option<String>,
    program_path: Option<String>,
    miden_client_path: Option<String>,
    miden_debug_path: Option<String>,
    python_path: Option<String>,
    account_id: Option<String>,
    inputs_path: Option<String>,
    entrypoint: Option<String>,
    program_args: Vec<String>,
    sysroot: Option<String>,
    search_path: Vec<String>,
    link_libraries: Vec<String>,
    source_path_prefixes: Vec<String>,
    trim_path_prefixes: Vec<String>,
    compiler_args: Vec<String>,
    midenc_args: Vec<String>,
    build_args: Vec<String>,
    cargo_miden_args: Vec<String>,
    cwd: Option<String>,
}

impl MidenDebugConfig {
    fn from_json(value: &zed::serde_json::Value) -> Self {
        let str_field = |k: &str| value.get(k).and_then(|v| v.as_str()).map(String::from);
        let str_array_field = |k: &str| {
            value
                .get(k)
                .and_then(zed::serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| match item {
                            zed::serde_json::Value::String(value) => Some(value.clone()),
                            zed::serde_json::Value::Number(value) => Some(value.to_string()),
                            zed::serde_json::Value::Bool(value) => Some(value.to_string()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            runtime: str_field("runtime"),
            host: str_field("host"),
            port: value
                .get("port")
                .and_then(zed::serde_json::Value::as_u64)
                .and_then(|n| u16::try_from(n).ok()),
            script_path: str_field("scriptPath"),
            program_path: str_field("programPath"),
            miden_client_path: str_field("midenClientPath"),
            miden_debug_path: str_field("midenDebugPath"),
            python_path: str_field("pythonPath"),
            account_id: str_field("accountId"),
            inputs_path: str_field("inputsPath"),
            entrypoint: str_field("entrypoint"),
            program_args: str_array_field("programArgs"),
            sysroot: str_field("sysroot"),
            search_path: str_array_field("searchPath"),
            link_libraries: str_array_field("linkLibraries"),
            source_path_prefixes: str_array_field("sourcePathPrefixes"),
            trim_path_prefixes: str_array_field("trimPathPrefixes"),
            compiler_args: str_array_field("compilerArgs"),
            midenc_args: str_array_field("midencArgs"),
            build_args: str_array_field("buildArgs"),
            cargo_miden_args: str_array_field("cargoMidenArgs"),
            cwd: str_field("cwd"),
        }
    }

    fn source_path_prefixes(&self, program_path: &str, cwd: Option<&str>) -> Vec<String> {
        let mut prefixes = BTreeSet::new();
        let extracted_prefixes = self.extract_trim_path_prefixes();
        for prefix in self
            .source_path_prefixes
            .iter()
            .chain(self.trim_path_prefixes.iter())
            .chain(extracted_prefixes.iter())
        {
            if let Some(path) = resolve_source_path_prefix(prefix, cwd) {
                prefixes.insert(path);
            }
        }
        if let Some(path) = infer_cargo_miden_package_root(program_path) {
            prefixes.insert(path);
        }
        prefixes.into_iter().collect()
    }

    fn extract_trim_path_prefixes(&self) -> Vec<String> {
        let mut args = Vec::new();
        args.extend(self.compiler_args.iter().cloned());
        args.extend(self.midenc_args.iter().cloned());
        args.extend(self.build_args.iter().cloned());
        args.extend(self.cargo_miden_args.iter().cloned());

        let mut prefixes = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "-Z" {
                if let Some(next) = args.get(i + 1)
                    && let Some(value) = next.strip_prefix("trim-path-prefix=")
                {
                    prefixes.push(value.to_string());
                    i += 2;
                    continue;
                }
            } else if let Some(value) = arg.strip_prefix("-Ztrim-path-prefix=") {
                prefixes.push(value.to_string());
            } else if (arg == "--trim-path-prefix" || arg == "--source-path-prefix")
                && args.get(i + 1).is_some()
            {
                prefixes.push(args[i + 1].clone());
                i += 2;
                continue;
            } else if let Some(value) = arg
                .strip_prefix("--trim-path-prefix=")
                .or_else(|| arg.strip_prefix("--source-path-prefix="))
            {
                prefixes.push(value.to_string());
            }

            i += 1;
        }

        prefixes
    }
}

fn debug_config_value(value: &zed::serde_json::Value) -> &zed::serde_json::Value {
    value
        .get("config")
        .filter(|config| config.is_object())
        .unwrap_or(value)
}

fn resolve_source_path_prefix(prefix: &str, cwd: Option<&str>) -> Option<String> {
    if prefix.is_empty() {
        return None;
    }
    let path = Path::new(prefix);
    if path.is_absolute() {
        Some(
            normalize_path(PathBuf::from(path))
                .to_string_lossy()
                .to_string(),
        )
    } else {
        Some(resolve_config_path(prefix, cwd?))
    }
}

fn infer_cargo_miden_package_root(program_path: &str) -> Option<String> {
    let normalized = Path::new(program_path);
    let components = normalized
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();

    let target_index = components
        .windows(2)
        .position(|window| window[0] == "target" && window[1] == "miden")?;
    if target_index == 0 {
        return None;
    }

    let mut root = PathBuf::new();
    for component in &components[..target_index] {
        root.push(component);
    }
    Some(normalize_path(root).to_string_lossy().to_string())
}

fn push_optional_arg(args: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        args.push(flag.to_string());
        args.push(value);
    }
}

fn push_optional_path_arg(args: &mut Vec<String>, flag: &str, value: Option<String>, cwd: &str) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        args.push(flag.to_string());
        args.push(resolve_config_path(&value, cwd));
    }
}

fn resolve_config_path(path: &str, cwd: &str) -> String {
    let path = Path::new(path);
    let resolved = if path.is_absolute() {
        PathBuf::from(path)
    } else {
        PathBuf::from(cwd).join(path)
    };
    normalize_path(resolved).to_string_lossy().to_string()
}

fn resolve_command_path(command: &str, cwd: &str) -> String {
    if command.contains('/') || command.starts_with('.') {
        resolve_config_path(command, cwd)
    } else {
        command.to_string()
    }
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push("..");
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn proxy_arguments(
    host: &str,
    port: u16,
    timeout_ms: u64,
    adapter_cwd: Option<&str>,
    ui_source_path: Option<&str>,
    adapter_argv: Option<Vec<String>>,
) -> Vec<String> {
    let mut args = vec![
        "-u".to_string(),
        "-c".to_string(),
        DAP_TCP_PROXY.to_string(),
        "--host".to_string(),
        host.to_string(),
        "--port".to_string(),
        port.to_string(),
        "--timeout-ms".to_string(),
        timeout_ms.to_string(),
    ];

    if let Some(adapter_cwd) = adapter_cwd {
        args.push("--adapter-cwd".to_string());
        args.push(adapter_cwd.to_string());
    }

    if let Some(ui_source_path) = ui_source_path {
        args.push("--source-path-for-ui".to_string());
        args.push(ui_source_path.to_string());
    }

    if let Some(adapter_argv) = adapter_argv {
        args.push("--adapter-argv".to_string());
        args.extend(adapter_argv);
    }

    args
}

/// Validate and pack a dotted-quad IPv4 string into octets in big-endian order.
fn parse_ipv4(addr: &str) -> Result<u32> {
    let octets: Vec<&str> = addr.split('.').collect();
    if octets.len() != 4 {
        return Err(format!("invalid IPv4 address: {addr}"));
    }
    let mut bytes = [0u8; 4];
    for (i, octet) in octets.iter().enumerate() {
        bytes[i] = octet
            .parse()
            .map_err(|_| format!("invalid IPv4 octet `{octet}` in {addr}"))?;
    }
    Ok(u32::from_be_bytes(bytes))
}

zed::register_extension!(MidenExtension);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debugger_launch_prefixes_include_explicit_trim_and_package_root() {
        let config = MidenDebugConfig::from_json(&zed::serde_json::json!({
            "runtime": "debugger",
            "programPath": "/workspace/examples/fibonacci/target/miden/debug/fibonacci.masp",
            "sourcePathPrefixes": ["src"],
            "trimPathPrefixes": ["/explicit/prefix"],
            "compilerArgs": ["-Ztrim-path-prefix=/compiler/prefix"],
            "midencArgs": ["-Z", "trim-path-prefix=midenc-prefix"],
            "buildArgs": ["--source-path-prefix", "build-prefix"],
            "cargoMidenArgs": ["--trim-path-prefix=cargo-prefix"],
            "cwd": "/workspace/examples/fibonacci"
        }));

        let prefixes = config.source_path_prefixes(
            "/workspace/examples/fibonacci/target/miden/debug/fibonacci.masp",
            Some("/workspace/examples/fibonacci"),
        );

        assert!(prefixes.contains(&"/workspace/examples/fibonacci/src".to_string()));
        assert!(prefixes.contains(&"/explicit/prefix".to_string()));
        assert!(prefixes.contains(&"/compiler/prefix".to_string()));
        assert!(prefixes.contains(&"/workspace/examples/fibonacci/midenc-prefix".to_string()));
        assert!(prefixes.contains(&"/workspace/examples/fibonacci/build-prefix".to_string()));
        assert!(prefixes.contains(&"/workspace/examples/fibonacci/cargo-prefix".to_string()));
        assert!(prefixes.contains(&"/workspace/examples/fibonacci".to_string()));
    }

    #[test]
    fn inferred_package_root_requires_target_miden_layout() {
        assert_eq!(
            infer_cargo_miden_package_root(
                "/workspace/examples/fibonacci/target/miden/debug/fibonacci.masp",
            ),
            Some("/workspace/examples/fibonacci".to_string()),
        );
        assert_eq!(infer_cargo_miden_package_root("/tmp/fibonacci.masp"), None);
    }

    #[test]
    fn resolves_debugger_paths_relative_to_launch_cwd() {
        let cwd = resolve_config_path("examples/fibonacci", "/workspace/compiler");

        assert_eq!(cwd, "/workspace/compiler/examples/fibonacci");
        assert_eq!(
            resolve_config_path("target/miden/debug/fibonacci.masp", &cwd),
            "/workspace/compiler/examples/fibonacci/target/miden/debug/fibonacci.masp"
        );
        assert_eq!(
            resolve_command_path("../../../miden-debug/target/debug/miden-debug", &cwd),
            "/workspace/miden-debug/target/debug/miden-debug"
        );
        assert_eq!(resolve_command_path("miden-debug", &cwd), "miden-debug");
        assert_eq!(
            resolve_config_path("../../target/debug/build/miden-core-lib/out/assets", &cwd,),
            "/workspace/compiler/target/debug/build/miden-core-lib/out/assets"
        );
    }

    #[test]
    fn unwraps_zed_scenario_config_wrapper() {
        let wrapper = zed::serde_json::json!({
            "label": "Debug Fibonacci",
            "adapter": "miden",
            "config": {
                "request": "launch",
                "runtime": "debugger",
                "programPath": "target/miden/debug/fibonacci.masp"
            }
        });

        let config = debug_config_value(&wrapper);
        assert_eq!(
            config
                .get("request")
                .and_then(zed::serde_json::Value::as_str),
            Some("launch")
        );
        assert_eq!(
            MidenDebugConfig::from_json(config).program_path.as_deref(),
            Some("target/miden/debug/fibonacci.masp")
        );
    }
}
