//! Settings kept in a file, for what would otherwise be typed or exported every time.
//!
//! The file holds `CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF` lines under the names the
//! environment uses, and is the one the Python command line reads. The environment
//! overrides the file, and the command line overrides both.

use std::path::PathBuf;

/// Every name the file may set; the service has no use for the host, the command line has.
pub const NAMES: [&str; 6] = [
    "CHSMARTBULB_ADDRESS",
    "CHSMARTBULB_TOKEN",
    "CHSMARTBULB_HOST",
    "CHSMARTBULB_TRANSPORT",
    "CHSMARTBULB_AUDIO_DEVICE",
    "CHSMARTBULB_MONITOR",
];

pub fn default_path() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        var("APPDATA").or_else(|| Some(var("USERPROFILE")?.join("AppData/Roaming")))
    } else {
        var("XDG_CONFIG_HOME").or_else(|| Some(var("HOME")?.join(".config")))
    };
    Some(base?.join("chsmartbulb/config"))
}

/// The settings in `text`; `source` names it in the error for a line that sets none of them.
pub fn parse(text: &str, source: &str) -> Result<Vec<(&'static str, String)>, String> {
    let mut values = Vec::new();
    for (number, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let known = line.split_once('=').and_then(|(name, value)| {
            let name = NAMES.iter().find(|known| **known == name.trim())?;
            Some((*name, value.trim()))
        });
        let Some((name, value)) = known else {
            return Err(format!("{source}:{}: expected one of {} followed by =VALUE", number + 1, NAMES.join(", ")));
        };
        let quoted = value.len() >= 2
            && [('"', '"'), ('\'', '\'')].contains(&(value.chars().next().unwrap(), value.chars().last().unwrap()));
        let value = if quoted { &value[1..value.len() - 1] } else { value };
        values.push((name, value.to_string()));
    }
    Ok(values)
}

/// Put the file's settings where the environment has none, so the options that name a
/// variable find them. Call it before anything else runs: it changes the environment.
pub fn apply() -> Result<(), String> {
    let Some(path) = default_path() else { return Ok(()) };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    for (name, value) in parse(&text, &path.to_string_lossy())? {
        if !value.is_empty() && std::env::var_os(name).is_none() {
            std::env::set_var(name, value);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_read_with_comments_quotes_and_export() {
        let text =
            "\u{feff}# the bulb\nCHSMARTBULB_ADDRESS = AA:BB:CC:DD:EE:FF\n\nexport CHSMARTBULB_TOKEN=\"s3 cret\"\n\
                    CHSMARTBULB_AUDIO_DEVICE='a=b'\nCHSMARTBULB_MONITOR=\n";
        assert_eq!(
            parse(text, "config").unwrap(),
            [
                ("CHSMARTBULB_ADDRESS", "AA:BB:CC:DD:EE:FF".to_string()),
                ("CHSMARTBULB_TOKEN", "s3 cret".to_string()),
                ("CHSMARTBULB_AUDIO_DEVICE", "a=b".to_string()),
                ("CHSMARTBULB_MONITOR", String::new()),
            ]
        );
    }

    #[test]
    fn a_line_that_sets_nothing_known_is_named() {
        for line in ["ADDRESS=1", "CHSMARTBULB_ADDRESS", "CHSMARTBULB_COLOUR=red"] {
            let error = parse(&format!("# first\n{line}\n"), "config").unwrap_err();
            assert!(error.starts_with("config:2: expected one of CHSMARTBULB_ADDRESS"), "{error}");
        }
    }
}
