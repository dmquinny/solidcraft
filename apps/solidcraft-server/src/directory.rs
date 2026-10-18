//! The identity provider's directory: with `SOLIDCRAFT_AUTHENTIK_URL` and
//! `SOLIDCRAFT_AUTHENTIK_TOKEN` set, the service reads Authentik's users through its API
//! (`/api/v3/core/users/`, a token with read access to users), so everyone appears in the
//! Users page and the Sharing window before they ever sign in, and superusers and the members
//! of `SOLIDCRAFT_ADMIN_GROUP` are admins. The list is refreshed at most once a minute, in the
//! request that needs it; a failure keeps the last list and is logged.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

const REFRESH_EVERY: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(10);

/// One person in the directory.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub name: String,
    pub email: String,
    pub admin: bool,
}

/// When fetched, and the people.
type Fetched = (Instant, Vec<Person>);

pub struct Directory {
    url: String,
    token: String,
    admin_group: String,
    cache: Mutex<Option<Fetched>>,
}

impl Directory {
    /// `None` without a URL and a token.
    pub fn new(url: &str, token: &str, admin_group: &str) -> Option<Self> {
        if url.trim().is_empty() || token.trim().is_empty() {
            return None;
        }
        Some(Self {
            url: url.trim().trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            admin_group: admin_group.to_string(),
            cache: Mutex::new(None),
        })
    }

    pub fn source(&self) -> &str {
        &self.url
    }

    /// Everyone active in the directory, from the cache or a fresh fetch.
    pub fn users(&self) -> Vec<Person> {
        let Ok(mut cache) = self.cache.lock() else { return Vec::new() };
        if let Some((at, users)) = cache.as_ref()
            && at.elapsed() < REFRESH_EVERY
        {
            return users.clone();
        }
        match self.fetch() {
            Ok(users) => {
                *cache = Some((Instant::now(), users.clone()));
                users
            }
            Err(e) => {
                log::warn!("directory: {e}");
                if let Some((at, _)) = cache.as_mut() {
                    *at = Instant::now();
                }
                cache.as_ref().map(|(_, u)| u.clone()).unwrap_or_default()
            }
        }
    }

    pub fn is_admin(&self, name: &str) -> bool {
        self.users().iter().any(|p| p.name == name && p.admin)
    }

    /// The user name behind a name a proxy passed: an email (Cloudflare Access) becomes the
    /// directory's user name; anything else stays as it is.
    pub fn resolve(&self, name: &str) -> String {
        if !name.contains('@') {
            return name.to_string();
        }
        self.users().iter().find(|p| p.email.eq_ignore_ascii_case(name)).map_or_else(|| name.to_string(), |p| p.name.clone())
    }

    fn fetch(&self) -> Result<Vec<Person>, String> {
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let url = format!("{}/api/v3/core/users/?page_size=200&is_active=true&page={page}", self.url);
            let text = ureq::get(&url)
                .header("Authorization", &format!("Bearer {}", self.token))
                .header("Accept", "application/json")
                .config()
                .timeout_global(Some(TIMEOUT))
                .build()
                .call()
                .map_err(|e| format!("{url}: {e}"))?
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("{url}: {e}"))?;
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("{url}: not JSON: {e}"))?;
            out.extend(parse_users(&v, &self.admin_group));
            let next = v["pagination"]["next"].as_u64().unwrap_or(0);
            if next == 0 || next <= page || page > 50 {
                break;
            }
            page = next;
        }
        out.sort_by_key(|p| p.name.to_lowercase());
        out.dedup_by(|a, b| a.name == b.name);
        Ok(out)
    }
}

/// The people in one page of Authentik's users answer: service accounts are left out; a
/// superuser, or a member of `admin_group`, is an admin.
fn parse_users(v: &Value, admin_group: &str) -> Vec<Person> {
    let Some(results) = v["results"].as_array() else { return Vec::new() };
    results
        .iter()
        .filter(|u| u["is_active"].as_bool().unwrap_or(true))
        .filter(|u| !u["type"].as_str().unwrap_or("").contains("service_account"))
        .filter_map(|u| {
            let name = u["username"].as_str()?.to_string();
            if crate::users::check_name(&name).is_err() {
                return None;
            }
            let groups = u["groups_obj"].as_array().map(|g| g.iter().filter_map(|g| g["name"].as_str()).any(|g| g == admin_group)).unwrap_or(false);
            let email = u["email"].as_str().unwrap_or("").to_string();
            Some(Person { name, email, admin: u["is_superuser"].as_bool().unwrap_or(false) || groups })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentik_users_are_read() {
        let v: Value = serde_json::from_str(
            r#"{"pagination":{"next":0},"results":[
                {"username":"akadmin","email":"ben@example.com","is_active":true,"is_superuser":true,"type":"internal","groups_obj":[{"name":"authentik Admins"}]},
                {"username":"catherine@example.com","email":"catherine@example.com","is_active":true,"is_superuser":false,"type":"internal","groups_obj":[{"name":"solidcraft-admins"}]},
                {"username":"stephan","email":"Stephan@example.com","is_active":true,"is_superuser":false,"type":"external","groups_obj":[]},
                {"username":"old","is_active":false,"is_superuser":false,"type":"internal","groups_obj":[]},
                {"username":"outpost-bot","is_active":true,"is_superuser":false,"type":"internal_service_account","groups_obj":[]},
                {"username":"bad name!","is_active":true,"is_superuser":false,"type":"internal","groups_obj":[]}
            ]}"#,
        )
        .unwrap();
        let users = parse_users(&v, "solidcraft-admins");
        let names: Vec<(&str, bool)> = users.iter().map(|p| (p.name.as_str(), p.admin)).collect();
        assert_eq!(names, vec![("akadmin", true), ("catherine@example.com", true), ("stephan", false)]);
        assert_eq!(users[0].email, "ben@example.com");
        assert!(Directory::new("", "x", "g").is_none() && Directory::new("http://a", "", "g").is_none());
        // An email from Cloudflare Access becomes the user name, whatever its case.
        let d = Directory::new("http://a", "t", "g").unwrap();
        *d.cache.lock().unwrap() = Some((Instant::now(), users));
        assert_eq!(d.resolve("BEN@example.com"), "akadmin");
        assert_eq!(d.resolve("stephan@example.com"), "stephan");
        assert_eq!(d.resolve("nobody@example.com"), "nobody@example.com");
        assert_eq!(d.resolve("akadmin"), "akadmin");
    }
}
