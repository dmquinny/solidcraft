//! The users file nginx signs people in against (`auth_basic_user_file`): one line per user,
//! `name:{SSHA}…` (the salted SHA-1 form nginx understands on every platform), with `:admin`
//! as the third field for those who may manage users. The command line and the web app's
//! Users window (`http`) both edit it; the first user ever added is an admin.

use std::io::{BufRead, IsTerminal, Write};
use std::path::Path;

use base64::Engine as _;
use sha1::{Digest, Sha1};

/// Shortest password accepted.
pub const MIN_PASSWORD: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub name: String,
    pub admin: bool,
}

/// `user add NAME [--admin] [--password-stdin]`, `user remove NAME`, `user list`.
pub fn command(file: &Path, args: &[String]) -> Result<(), String> {
    let name = args.get(1).filter(|a| !a.starts_with("--")).map(String::as_str);
    match (args.first().map(String::as_str), name) {
        (Some("add"), Some(name)) => {
            let password = if args.iter().any(|a| a == "--password-stdin") { read_line()? } else { ask_password()? };
            let admin = args.iter().any(|a| a == "--admin");
            set(file, name, &password, admin.then_some(true))?;
            println!("{name}: added to {}{}", file.display(), if is_admin(file, name) { " (admin)" } else { "" });
            Ok(())
        }
        (Some("remove"), Some(name)) => {
            remove(file, name)?;
            println!("{name}: removed");
            Ok(())
        }
        (Some("list"), None) => {
            for u in list(file)? {
                println!("{}{}", u.name, if u.admin { " (admin)" } else { "" });
            }
            Ok(())
        }
        _ => Err(crate::USAGE.to_string()),
    }
}

/// Every user.
pub fn list(file: &Path) -> Result<Vec<User>, String> {
    Ok(read(file)?.into_iter().map(|(name, _, admin)| User { name, admin }).collect())
}

pub fn is_admin(file: &Path, name: &str) -> bool {
    read(file).ok().is_some_and(|us| us.iter().any(|(n, _, a)| n == name && *a))
}

/// Add a user, or give an existing one a new password. `admin`: `Some` sets the flag, `None`
/// keeps it (a new user is not an admin, unless the file was empty: the first user is).
pub fn set(file: &Path, name: &str, password: &str, admin: Option<bool>) -> Result<(), String> {
    check_name(name)?;
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!("the password needs at least {MIN_PASSWORD} characters"));
    }
    let mut entries = read(file)?;
    let first = entries.is_empty();
    let was = entries.iter().find(|(n, _, _)| n == name).map(|(_, _, a)| *a);
    entries.retain(|(n, _, _)| n != name);
    entries.push((name.to_string(), hash(password), admin.or(was).unwrap_or(first)));
    write(file, &entries)
}

/// Make a user an admin, or not (never the last admin).
pub fn set_admin(file: &Path, name: &str, admin: bool) -> Result<(), String> {
    let mut entries = read(file)?;
    let Some(i) = entries.iter().position(|(n, _, _)| n == name) else { return Err(format!("no user named {name}")) };
    if !admin && entries[i].2 && entries.iter().filter(|(_, _, a)| *a).count() == 1 {
        return Err("that is the only admin; make someone else an admin first".into());
    }
    entries[i].2 = admin;
    write(file, &entries)
}

/// Remove a user (never the last admin).
pub fn remove(file: &Path, name: &str) -> Result<(), String> {
    let mut entries = read(file)?;
    let Some((_, _, admin)) = entries.iter().find(|(n, _, _)| n == name) else { return Err(format!("no user named {name}")) };
    if *admin && entries.iter().filter(|(_, _, a)| *a).count() == 1 {
        return Err("that is the only admin; make someone else an admin first".into());
    }
    entries.retain(|(n, _, _)| n != name);
    write(file, &entries)
}

/// Names nginx and URLs are happy with.
pub fn check_name(name: &str) -> Result<(), String> {
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

/// (name, hash, admin) per line.
fn read(file: &Path) -> Result<Vec<(String, String, bool)>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    Ok(text
        .lines()
        .filter_map(|l| {
            let mut f = l.splitn(3, ':');
            let (name, hash) = (f.next()?.trim(), f.next()?.trim());
            let admin = f.next().is_some_and(|c| c.trim() == "admin");
            (!name.is_empty()).then(|| (name.to_string(), hash.to_string(), admin))
        })
        .collect())
}

/// Written whole, through a temporary file, readable by nginx's user.
fn write(file: &Path, entries: &[(String, String, bool)]) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = file.with_extension("tmp");
    let mut out = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    for (n, h, admin) in entries {
        writeln!(out, "{n}:{h}{}", if *admin { ":admin" } else { "" }).map_err(|e| e.to_string())?;
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    std::fs::rename(&tmp, file).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_user_is_the_admin_and_the_last_admin_stays() {
        let file = std::env::temp_dir().join(format!("solidcraft-users-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        set(&file, "ben", "secret-1", None).unwrap();
        set(&file, "felix", "secret-2", None).unwrap();
        assert!(is_admin(&file, "ben") && !is_admin(&file, "felix"));
        assert!(set(&file, "x", "short", None).is_err());
        assert!(set(&file, "bad name", "secret-3", None).is_err());
        assert!(remove(&file, "ben").is_err(), "the only admin stays");
        set(&file, "felix", "secret-2", Some(true)).unwrap();
        remove(&file, "ben").unwrap();
        assert_eq!(list(&file).unwrap(), vec![User { name: "felix".into(), admin: true }]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("felix:{SSHA}") && text.trim_end().ends_with(":admin"), "{text}");
        let _ = std::fs::remove_file(&file);
    }
}
