use zed_extension_api::{self as zed, settings::LspSettings, LanguageServerId, Result};

const SERVER_NAME: &str = "miden-lsp";

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
            return Err(format!("unknown language server ID {}", language_server_id.as_ref()));
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
}

zed::register_extension!(MidenExtension);
