//! The users file nginx signs people in against (`auth_basic_user_file`): one `name:{SSHA}…`
//! line per user, the salted SHA-1 form nginx understands on every platform.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use base64::Engine as _;
use sha1::{Digest, Sha1};

/// `user add|remove|list [NAME] [--password-stdin]`.
pub fn command(file: &Path, args: &[String]) -> Result<(), String> {
    let name = args.get(1).map(String::as_str);
    match (args.first().map(String::as_str), name) {
        (Some("add"), Some(name)) => {
            check_name(name)?;
            let password = if args.iter().any(|a| a == "--password-stdin") { read_line()? } else { ask_password()? };
            if password.len() < 4 {
                return Err("the password needs at least 4 characters".into());
            }
            let mut entries = read(file)?;
            entries.retain(|(n, _)| n != name);
            entries.push((name.to_string(), hash(&password)));
            write(file, &entries)?;
            println!("{name}: added to {}", file.display());
            Ok(())
        }
        (Some("remove"), Some(name)) => {
            let mut entries = read(file)?;
            let before = entries.len();
            entries.retain(|(n, _)| n != name);
            if entries.len() == before {
                return Err(format!("no user named {name}"));
            }
            write(file, &entries)?;
            println!("{name}: removed");
            Ok(())
        }
        (Some("list"), None) => {
            for (n, _) in read(file)? {
                println!("{n}");
            }
            Ok(())
        }
        _ => Err(crate::USAGE.to_string()),
    }
}

/// Names nginx and URLs are happy with.
fn check_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok { Ok(()) } else { Err("a user name is 1 to 32 letters, digits, '.', '_' or '-'".into()) }
}

fn read_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn ask_password() -> Result<String, String> {
    if !std::io::stdin().is_terminal() {
        return read_line();
    }
    let first = rpassword::prompt_password("Password: ").map_err(|e| e.to_string())?;
    let again = rpassword::prompt_password("Again: ").map_err(|e| e.to_string())?;
    if first != again {
        return Err("the passwords differ".into());
    }
    Ok(first)
}

/// `{SSHA}` + base64(sha1(password + salt) + salt).
fn hash(password: &str) -> String {
    let salt = salt();
    let mut h = Sha1::new();
    h.update(password.as_bytes());
    h.update(salt);
    let mut out = h.finalize().to_vec();
    out.extend_from_slice(&salt);
    format!("{{SSHA}}{}", base64::engine::general_purpose::STANDARD.encode(out))
}

/// Eight bytes nobody can guess: the system's random source, else a mix of time and pid.
fn salt() -> [u8; 8] {
    let mut salt = [0u8; 8];
    let random = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut salt));
    if random.is_err() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let mut x = (nanos as u64) ^ (u64::from(std::process::id()) << 32) ^ 0x9E37_79B9_7F4A_7C15;
        for b in &mut salt {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = (x >> 24) as u8;
        }
    }
    salt
}

fn read(file: &Path) -> Result<Vec<(String, String)>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    Ok(text
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(n, h)| (n.trim().to_string(), h.trim().to_string()))
        .filter(|(n, _)| !n.is_empty())
        .collect())
}

/// Written whole, through a temporary file, readable by nginx's user.
fn write(file: &Path, entries: &[(String, String)]) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = file.with_extension("tmp");
    let mut out = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    for (n, h) in entries {
        writeln!(out, "{n}:{h}").map_err(|e| e.to_string())?;
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
}
