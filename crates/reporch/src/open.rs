//! Platform URL opening for the top-level star action (§5.1).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCommand {
    pub program: String,
    pub args: Vec<String>,
}

pub fn command_for(url: &str, platform: &str) -> Result<OpenCommand, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("only http:// and https:// URLs may be opened".into());
    }
    let program = match platform {
        "macos" => "open",
        "linux" => "xdg-open",
        other => return Err(format!("opening a browser is unsupported on {other}")),
    };
    Ok(OpenCommand {
        program: program.into(),
        args: vec![url.into()],
    })
}

/// Open through the platform command with the URL as one argument. No shell parses the URL.
pub fn open_url(url: &str) -> Result<(), String> {
    let command = command_for(url, std::env::consts::OS)?;
    let status = std::process::Command::new(&command.program)
        .args(&command.args)
        .status()
        .map_err(|error| format!("could not run `{}`: {error}", command.program))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{}` exited with {}",
            command.program,
            status
                .code()
                .map_or_else(|| "a signal".into(), |code| code.to_string())
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opener_is_a_direct_argument_not_a_shell_command() {
        let command = command_for("https://github.com/sezeryavuz/reporch", "macos").unwrap();
        assert_eq!(command.program, "open");
        assert_eq!(command.args, vec!["https://github.com/sezeryavuz/reporch"]);

        let linux = command_for("https://github.com/sezeryavuz/reporch", "linux").unwrap();
        assert_eq!(linux.program, "xdg-open");
        assert_eq!(linux.args.len(), 1);
    }

    #[test]
    fn non_web_urls_and_unsupported_platforms_are_rejected() {
        assert!(command_for("file:///etc/passwd", "linux").is_err());
        assert!(command_for("https://example.com", "windows").is_err());
    }
}
