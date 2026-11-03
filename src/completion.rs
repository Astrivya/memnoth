//! Shell completion commands for memnoth.

use anyhow::{Context, Result};
use clap::CommandFactory;
use clap_complete::aot::generate;
use std::fs;
use std::io;
use std::path::PathBuf;

pub use clap_complete::aot::Shell;


/// Build the root clap command for generating completions.
fn build_cli() -> clap::Command {
    crate::cli::Cli::command().bin_name("memnoth")
}

/// Detect the user's current shell from the SHELL environment variable.
pub fn detect_shell() -> Option<Shell> {
    Shell::from_env()
}

/// Get the standard completion directory for a given shell.
fn completion_dir(shell: Shell) -> Result<PathBuf> {
    match shell {
        Shell::Bash => {
            // bash: ~/.local/share/bash-completion/completions or /usr/share/bash-completion/completions
            if let Some(home) = dirs::home_dir() {
                Ok(home.join(".local/share/bash-completion/completions"))
            } else {
                Err(anyhow::anyhow!("Could not determine home directory"))
            }
        }
        Shell::Zsh => {
            // zsh: ~/.zsh/completions or any directory in $fpath
            if let Some(home) = dirs::home_dir() {
                Ok(home.join(".zsh/completions"))
            } else {
                Err(anyhow::anyhow!("Could not determine home directory"))
            }
        }
        Shell::Fish => {
            // fish: ~/.config/fish/completions
            if let Some(home) = dirs::home_dir() {
                Ok(home.join(".config/fish/completions"))
            } else {
                Err(anyhow::anyhow!("Could not determine home directory"))
            }
        }
        Shell::PowerShell => {
            // PowerShell: ~/.config/powershell/ or ~/Documents/WindowsPowerShell/
            if let Some(home) = dirs::home_dir() {
                #[cfg(windows)]
                {
                    Ok(home.join("Documents/WindowsPowerShell"))
                }
                #[cfg(not(windows))]
                {
                    Ok(home.join(".config/powershell"))
                }
            } else {
                Err(anyhow::anyhow!("Could not determine home directory"))
            }
        }
        Shell::Elvish => {
            // Elvish: ~/.config/elvish/lib
            if let Some(home) = dirs::home_dir() {
                Ok(home.join(".config/elvish/lib"))
            } else {
                Err(anyhow::anyhow!("Could not determine home directory"))
            }
        }
        _ => Err(anyhow::anyhow!("Unsupported shell: {:?}", shell)),
    }
}

/// Get the completion file name for a given shell.
fn completion_filename(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => "memnoth.bash",
        Shell::Zsh => "_memnoth",
        Shell::Fish => "memnoth.fish",
        Shell::PowerShell => "memnoth.ps1",
        Shell::Elvish => "memnoth.elv",
        _ => "memnoth",
    }
}

/// Print completions to stdout.
pub fn print_completions(shell: Shell) -> Result<()> {
    let mut cmd = build_cli();
    generate(shell, &mut cmd, "memnoth", &mut io::stdout());
    Ok(())
}

/// Install completions to the standard location for the given shell.
pub fn install_completions(shell: Shell) -> Result<PathBuf> {
    let dir = completion_dir(shell)
        .with_context(|| format!("Failed to find completion directory for {:?}", shell))?;

    // Create directory if it doesn't exist
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create completion directory: {}", dir.display()))?;

    let file_path = dir.join(completion_filename(shell));
    let mut file = fs::File::create(&file_path)
        .with_context(|| format!("Failed to create completion file: {}", file_path.display()))?;

    let mut cmd = build_cli();
    generate(shell, &mut cmd, "memnoth", &mut file);

    Ok(file_path)
}

/// Show completions for a specific shell (prints to stdout).
pub fn show_completions(shell: Shell) -> Result<()> {
    print_completions(shell)
}

/// Install completions for the detected or specified shell.
pub fn install(shell_opt: Option<Shell>) -> Result<()> {
    let shell = shell_opt.or_else(detect_shell).unwrap_or(Shell::Bash);
    let path = install_completions(shell)?;
    println!(
        "Installed completions for {:?} to: {}",
        shell,
        path.display()
    );
    println!("\nTo enable completions, restart your shell or source the file:");
    match shell {
        Shell::Bash => {
            println!("  source {}", path.display());
            println!("Or add to ~/.bashrc:");
            println!("  source {}", path.display());
        }
        Shell::Zsh => {
            println!("  source {}", path.display());
            println!(r#"Or add to ~/.zshrc (ensure ~/.zsh/completions is in \$fpath):"#);
            println!(r#"  fpath=(~/.zsh/completions \$fpath)"#);
            println!("  autoload -U compinit && compinit");
        }
        Shell::Fish => {
            println!("Fish completions are auto-loaded from ~/.config/fish/completions/");
        }
        Shell::PowerShell => {
            #[cfg(windows)]
            {
                println!("Add to your PowerShell profile:");
                println!("  . '{}'", path.display());
            }
            #[cfg(not(windows))]
            {
                println!("Add to your PowerShell profile:");
                println!("  . '{}'", path.display());
            }
        }
        Shell::Elvish => {
            println!("Elvish completions are auto-loaded from ~/.config/elvish/lib/");
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_shell_from_env() {
        // This test just ensures the function exists and compiles
        let _ = detect_shell();
    }

    #[test]
    fn test_completion_filename() {
        assert_eq!(completion_filename(Shell::Bash), "memnoth.bash");
        assert_eq!(completion_filename(Shell::Zsh), "_memnoth");
        assert_eq!(completion_filename(Shell::Fish), "memnoth.fish");
        assert_eq!(completion_filename(Shell::PowerShell), "memnoth.ps1");
    }
}
