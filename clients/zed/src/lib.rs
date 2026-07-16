use zed_extension_api::{
    settings::LspSettings, Command, Extension, LanguageServerId, Result, Worktree,
};

struct MooseDevExtension;

impl Extension for MooseDevExtension {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<Command> {
        let settings = LspSettings::for_worktree("moosedev", worktree)?;
        if let Some(binary) = settings.binary {
            if let Some(path) = binary.path {
                return Ok(Command::new(path)
                    .args(binary.arguments.unwrap_or_else(|| vec!["lsp".into()])));
            }
        }

        if let Some(path) = worktree.which("moosedev") {
            return Ok(Command::new(path).arg("lsp"));
        }

        Err("install moosedev and ensure it is on PATH, or set lsp.moosedev.binary.path in Zed settings".into())
    }
}

zed_extension_api::register_extension!(MooseDevExtension);
