//! Where Command Center keeps its files: ~/.config/control-center and ~/.cache/control-center, by
//! $HOME, never $XDG_CONFIG_HOME. The Desktop session sets XDG_CONFIG_HOME to its own Plasma
//! config (~/.config/control-center/desktop), and everything here must still find the real one.
use std::path::PathBuf;

/// $HOME, or "." without one.
pub fn home_dir() -> String {
    home_or_dot(std::env::var("HOME").ok())
}

fn home_or_dot(home: Option<String>) -> String {
    home.unwrap_or_else(|| ".".into())
}

/// ~/.config/control-center/<name>
pub fn config(name: &str) -> String {
    format!("{}/.config/control-center/{name}", home_dir())
}

/// ~/.config/control-center
pub fn conf_dir() -> PathBuf {
    PathBuf::from(home_dir()).join(".config/control-center")
}

/// ~/.cache/control-center/<name>
pub fn cache(name: &str) -> String {
    format!("{}/.cache/control-center/{name}", home_dir())
}

#[cfg(test)]
mod tests {
    /// The folders hang off HOME, or "." without one.
    #[test]
    fn by_home_alone() {
        assert_eq!(super::home_or_dot(None), ".");
        let home = super::home_dir();
        assert_eq!(super::config("home.json"), format!("{home}/.config/control-center/home.json"));
        assert_eq!(super::conf_dir(), std::path::PathBuf::from(format!("{home}/.config/control-center")));
        assert_eq!(super::cache("scan"), format!("{home}/.cache/control-center/scan"));
    }
}
