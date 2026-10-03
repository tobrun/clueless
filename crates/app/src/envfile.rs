//! Finding and parsing the `.env` file with the `LLM_*` and `ASR_*`
//! settings. Parsing goes through `dotenvy::from_path_iter` into a map;
//! the process environment is never written to, so the file only fills
//! gaps where a variable is not already set (see `Config::load` callers
//! for the precedence).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The file to read: the explicit `--env-file` (a missing one is an error),
/// else the first existing of `./.env` and `~/.config/clueless/.env`. A
/// missing default is fine when the process environment carries the
/// variables anyway.
pub fn resolve(explicit: Option<&Path>) -> Result<Option<PathBuf>, String> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(format!(
                "env file not found at {}; copy .env.example to that path and fill it in",
                path.display()
            ));
        }
        return Ok(Some(path.to_path_buf()));
    }
    if let Some(local) = std::env::current_dir().ok().map(|dir| dir.join(".env"))
        && local.is_file()
    {
        return Ok(Some(local));
    }
    let config_home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(".config/clueless/.env"));
    Ok(config_home.filter(|path| path.is_file()))
}

/// The variables in the file; empty when there is no file to read.
pub fn load(explicit: Option<&Path>) -> Result<HashMap<String, String>, String> {
    let Some(path) = resolve(explicit)? else {
        return Ok(HashMap::new());
    };
    let mut vars = HashMap::new();
    for item in dotenvy::from_path_iter(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?
    {
        let (name, value) =
            item.map_err(|error| format!("could not parse {}: {error}", path.display()))?;
        vars.insert(name, value);
    }
    Ok(vars)
}

/// One lookup over two sources: `process` (the real environment) wins over
/// the parsed `file`, so an exported variable always beats the file.
pub fn overlay<'a, F>(
    process: F,
    file: &'a HashMap<String, String>,
) -> impl Fn(&str) -> Option<String> + 'a
where
    F: Fn(&str) -> Option<String> + 'a,
{
    move |name: &str| process(name).or_else(|| file.get(name).cloned())
}
