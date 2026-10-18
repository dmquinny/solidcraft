//! SolidCraft in the browser.
//!
//! Runs the same [`solidcraft_ui_egui::SolveApp`] as the desktop app through eframe's web runner
//! (wgpu: WebGPU where available, WebGL2 otherwise). Build with `trunk build --release` from this
//! directory (output in `dist/web`). URL flags: `?webgl` forces WebGL2 (remembered until
//! `?webgpu`), `?sample` opens the sample part.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
mod api;
#[cfg(target_arch = "wasm32")]
mod files;
#[cfg(target_arch = "wasm32")]
mod install;
#[cfg(any(target_arch = "wasm32", test))]
mod space_mouse;
#[cfg(target_arch = "wasm32")]
mod sync;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::rc::Rc;

    use solidcraft_engine::Session;
    use solidcraft_engine::io::vfs;
    use solidcraft_ui_egui::{HostPage, Services, SolveApp};
    use wasm_bindgen::JsCast as _;

    use crate::api::Api;
    use crate::files::{self, Files, Intent};
    use crate::space_mouse::SpaceMouse;

    const CANVAS_ID: &str = "solidcraft_canvas";
    const LOADING_ID: &str = "solidcraft_loading";
    /// A 32-bit float depth buffer (no stencil): WebGPU and WebGL2 both have it, and the viewport
    /// draws it with reversed depth, whose precision follows the distance from the eye (#1434).
    const DEPTH_BITS: u8 = 32;

    /// The loading animation (assets/styles/web/loading-logo.svg) settles into the finished mark
    /// about 3.2 s after the page starts; this adds a short hold. The app doesn't land before it.
    const LOADING_MIN_MS: f64 = 3600.0;
    /// The fade from the loading screen into the app.
    const LOADING_FADE_MS: i32 = 300;

    /// Take the loading screen away once the animation has played to the finished mark: wait out
    /// what is left of `LOADING_MIN_MS`, fade it, then remove it.
    fn dismiss_loading(el: web_sys::Element) {
        let Some(window) = web_sys::window() else {
            el.remove();
            return;
        };
        let now = window.performance().map_or(LOADING_MIN_MS, |p| p.now());
        let wait = (LOADING_MIN_MS - now).max(0.0) as i32;
        let fade = wasm_bindgen::closure::Closure::once_into_js(move || {
            let _ = el.set_attribute("class", "done");
            let gone = wasm_bindgen::closure::Closure::once_into_js(move || el.remove());
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(gone.unchecked_ref(), LOADING_FADE_MS);
            }
        });
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(fade.unchecked_ref(), wait);
    }

    /// Seconds between autosaves of unsaved work.
    const AUTOSAVE_S: f64 = 30.0;

    /// The Users page's fields: a new user (name, password, admin) and this user's new password.
    #[derive(Default)]
    struct UsersForm {
        new_user: (String, String, bool),
        my_password: String,
    }

    /// The start page's "Shared with you" (files::open_shared): what others shared with this
    /// user by name, as cards like Recent's, opened with one click. Nothing is drawn when there
    /// is nothing to show.
    fn shared_section(app: &mut SolveApp, ui: &mut egui::Ui, files: &Rc<Files>) {
        let shared = files.shared.borrow().clone();
        if !files::remote_only() || shared.is_empty() {
            return;
        }
        ui.add_space(24.0);
        solidcraft_ui_egui::home::section(ui, "Shared with you");
        let mut picked: Option<files::Shared> = None;
        solidcraft_ui_egui::home::grid(ui, shared.len(), |ui, i| {
            let Some(s) = shared.get(i) else { return };
            let sub = format!("shared by {}{}", s.owner, if s.edit { "" } else { " (view only)" });
            let response = if s.folder {
                let name = if s.path.is_empty() { "The design folder".to_string() } else { format!("Folder: {}", s.path) };
                solidcraft_ui_egui::home::plain_card(ui, &name, &sub)
            } else {
                let name = s.path.rsplit_once('/').map_or(s.path.as_str(), |(_, n)| n).trim_end_matches(".solidcraft").to_string();
                solidcraft_ui_egui::home::design_card(app, ui, &format!("{}{}", files::SHARED, s.path), &name, &sub)
            };
            if response.on_hover_text(&s.path).clicked() {
                picked = Some(s.clone());
            }
        });
        if let Some(s) = picked {
            files::open_shared(files, &s);
        }
    }

    /// Preferences › Users (files::users): everyone changes their own password; admins add and
    /// remove users and say who else is an admin. Only with the storage service.
    fn users_page(ui: &mut egui::Ui, files: &Rc<Files>, form: &Rc<std::cell::RefCell<UsersForm>>) {
        if !files::remote_only() {
            ui.label("Designs are kept in this browser. Users apply when they are kept on the server.");
            return;
        }
        if files::remote_user().is_empty() {
            ui.label("This server keeps designs only, not users (plain nginx storage).");
            return;
        }
        if !files.users_asked.get() {
            files::users(files);
        }
        let Some(users) = files.users.borrow().clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Asking the server\u{2026}");
            });
            return;
        };
        let mut form = form.borrow_mut();
        let mut act: Option<(&'static str, String, bool)> = None;
        ui.horizontal(|ui| {
            ui.label(format!("Signed in as {}{}", users.me, if users.admin { " (admin)" } else { "" }));
            if ui.small_button("Refresh").clicked() {
                files::users(files);
            }
        });
        ui.horizontal(|ui| {
            ui.label("New password:");
            ui.add(egui::TextEdit::singleline(&mut form.my_password).password(true).desired_width(160.0));
            if ui.small_button("Change").clicked() && !form.my_password.is_empty() {
                act = Some(("password", std::mem::take(&mut form.my_password), false));
            }
        });
        ui.separator();
        ui.label("Who can sign in (everyone who has, with a proxy doing the sign-in):");
        egui::Grid::new("users").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
            for (name, admin) in &users.list {
                ui.label(name);
                if users.admin && *name != users.me {
                    let mut is_admin = *admin;
                    if ui.checkbox(&mut is_admin, "admin").changed() {
                        act = Some(("admin", name.clone(), is_admin));
                    }
                    if ui.small_button("Remove").clicked() {
                        act = Some(("remove", name.clone(), false));
                    }
                } else {
                    ui.label(if *admin { "admin" } else { "" });
                    ui.label("");
                }
                ui.end_row();
            }
        });
        if users.admin {
            ui.separator();
            ui.label("Add a user, or give one a new password:");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut form.new_user.0).hint_text("name").desired_width(120.0));
                ui.add(egui::TextEdit::singleline(&mut form.new_user.1).hint_text("password").password(true).desired_width(140.0));
                ui.checkbox(&mut form.new_user.2, "admin");
                if ui.small_button("Add").clicked() && !form.new_user.0.trim().is_empty() && !form.new_user.1.is_empty() {
                    act = Some(("add", form.new_user.0.trim().to_string(), form.new_user.2));
                }
            });
        }
        match act {
            Some(("password", password, _)) => files::set_password(files, &password),
            Some(("admin", name, admin)) => files::set_user(files, &name, None, Some(admin)),
            Some(("remove", name, _)) => files::remove_user(files, &name),
            Some(("add", name, admin)) => {
                let password = std::mem::take(&mut form.new_user.1);
                files::set_user(files, &name, Some(&password), Some(admin));
                form.new_user = Default::default();
            }
            _ => {}
        }
    }

    thread_local! {
        /// An open design has unsaved changes: leaving the page asks first.
        static UNSAVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// The browser's "leave site? changes you made may not be saved" prompt while a design has
    /// unsaved changes (the page can't show its own dialog when it is being closed).
    fn ask_before_unload(window: &web_sys::Window) {
        let on_unload = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(|e: web_sys::BeforeUnloadEvent| {
            if UNSAVED.with(std::cell::Cell::get) {
                e.prevent_default();
                e.set_return_value("unsaved");
            }
        });
        let _ = window.add_event_listener_with_callback("beforeunload", on_unload.as_ref().unchecked_ref());
        // The listener lives as long as the page.
        on_unload.forget();
    }

    const JAPANESE_FONT_PATH: &str = "./fonts/BIZUDPGothic-Regular.ttf";

    type FontLoadResult = Rc<std::cell::RefCell<Option<Result<Vec<u8>, String>>>>;

    struct Shell {
        app: SolveApp,
        api: Api,
        files: Rc<Files>,
        /// When the last autosave was (seconds), and the revision it holds (none stored: `None`).
        autosaved_at: f64,
        autosaved: Option<u64>,
        /// A stored design being renamed: (its name, the new name).
        renaming: Option<(String, String)>,
        /// The rename field takes the keyboard when it appears.
        rename_focus: bool,
        /// The file panel's folder field (server storage, files.rs).
        folder: String,
        /// Live collaboration on the open server design (sync.rs).
        sync: Rc<crate::sync::Sync>,
        /// The history window's branch name and merge target fields.
        branch_name: String,
        merge_target: String,
        /// The Sharing window's choices (`None`: everyone), and what they were made for.
        share_for: Option<files::Target>,
        share_view: Option<std::collections::BTreeSet<String>>,
        share_edit: Option<std::collections::BTreeSet<String>>,
        /// The start page's recent designs were filled from the stored ones.
        recents_synced: bool,
        /// On-demand CJK font: fetched only when a CJK language is selected.
        font: solidcraft_ui_egui::fonts::CjkFontLoader,
        font_result: FontLoadResult,
        space_mouse: SpaceMouse,
    }

    impl Shell {
        /// Files from the computer, writes to store, messages and the autosave.
        fn files_tick(&mut self, ctx: &egui::Context) {
            if self.files.ctx.borrow().is_none() {
                *self.files.ctx.borrow_mut() = Some(ctx.clone());
            }
            // A ready picker answer is consumed in app.logic later in this frame. Keep
            // incoming files queued until then so they cannot replace its target design.
            if self.app.file_picker_busy() {
                ctx.request_repaint();
                return;
            }
            files::flush(&self.files);
            for (text, error) in self.files.messages.borrow_mut().drain(..) {
                self.app.set_status(text, error);
            }
            let mut incoming: Vec<_> = self.files.incoming.borrow_mut().drain(..).collect();
            // A glTF file's buffers picked with it are kept next to it, not opened.
            incoming.retain_mut(|(name, bytes, _)| {
                let companion =
                    std::path::Path::new(name.as_str()).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("bin"));
                if companion {
                    vfs::insert(&format!("{}{name}", files::UPLOAD), std::mem::take(bytes));
                }
                !companion
            });
            for (name, bytes, intent) in incoming {
                let design = solidcraft_engine::io::is_design_path(&name);
                let path = if design && intent == Intent::Open {
                    // A design from the computer is kept in the browser (a copy), so Save works.
                    let path = files::free_name(&name);
                    if let Err(e) = vfs::write(&path, &bytes) {
                        self.app.set_status(e.to_string(), true);
                    }
                    files::flush(&self.files);
                    path
                } else {
                    let path = format!("{}{name}", files::UPLOAD);
                    vfs::insert(&path, bytes);
                    path
                };
                match intent {
                    Intent::Open => self.app.open_path(&path),
                    Intent::Insert => self.app.drop_path(&path),
                }
                self.files.panel.set(false);
            }
            if self.files.ready.get() && !self.recents_synced {
                self.recents_synced = true;
                self.sync_recents();
            }
            // A shared design picked on the start page opens once its folder's entries are in.
            if self.files.ready.get()
                && let Some(name) = self.files.open_next.borrow_mut().take()
            {
                let path = format!("{}{name}", files::OPFS);
                if vfs::exists(&path) {
                    self.app.open_path(&path);
                } else {
                    self.app.set_status(format!("{name}: not on the server any more"), true);
                }
            }
            let now = ctx.input(|i| i.time);
            // Unsaved work is kept (at most every AUTOSAVE_S); saved work needs no autosave.
            if self.files.ready.get() && self.files.recovery.borrow().is_none() {
                let s = &self.app.session;
                let changed = s.is_dirty() && self.autosaved != Some(s.revision);
                let wait = AUTOSAVE_S - (now - self.autosaved_at);
                if changed && wait <= 0.0 {
                    files::autosave(&self.files, Some((&s.design(), s.path.as_deref())));
                    self.autosaved = Some(s.revision);
                    self.autosaved_at = now;
                } else if changed {
                    // Come back for it even when nothing else happens.
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
                } else if !s.is_dirty() && self.autosaved.is_some() {
                    files::autosave(&self.files, None);
                    self.autosaved = None;
                }
            }
            if !self.files.ready.get() || !self.files.incoming.borrow().is_empty() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }

        /// The start page's recent designs are the stored ones: the remembered order first (those
        /// still stored), then the others, newest first.
        fn sync_recents(&mut self) {
            let mut entries = self.files.entries.borrow().clone();
            entries.sort_by(|a, b| b.modified.total_cmp(&a.modified));
            let recent = &mut self.app.home.recent;
            recent.retain(|p| vfs::exists(p));
            for e in entries.iter().filter(|e| solidcraft_engine::io::is_design_path(&e.name)) {
                let p = format!("{}{}", files::OPFS, e.name);
                if !recent.contains(&p) {
                    recent.push(p);
                }
            }
            recent.truncate(solidcraft_ui_egui::home::MAX_RECENT);
        }

        /// The stored designs: open, insert, rename, delete, download; open from the computer.
        fn file_panel(&mut self, ctx: &egui::Context) {
            if !self.files.panel.get() {
                return;
            }
            let mut open = true;
            let mut act: Option<(&'static str, String)> = None;
            let remote = files::remote_only();
            let user = files::remote_user();
            let title = match (remote, user.is_empty()) {
                (false, _) => "Files in this browser".to_string(),
                (true, true) => "Files on the server".to_string(),
                (true, false) => format!("Files on the server \u{b7} {user}"),
            };
            egui::Window::new(title)
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_width(520.0)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Open from computer…").clicked() {
                            files::pick_from_disk(&self.files, Intent::Open);
                        }
                        if ui.button("Insert from computer…").on_hover_text("STEP, 3MF or STL into the open design").clicked() {
                            files::pick_from_disk(&self.files, Intent::Insert);
                        }
                        if ui.button("Download open design").clicked() {
                            act = Some(("download_open", String::new()));
                        }
                        if remote
                            && !user.is_empty()
                            && ui.button("Users\u{2026}").on_hover_text("Who can sign in; your password (Preferences)").clicked()
                        {
                            act = Some(("users", String::new()));
                        }
                    });
                    if remote {
                        // The subfolder of the server's design folder designs are filed in (files.rs).
                        ui.horizontal_wrapped(|ui| {
                            let current = files::remote_folder();
                            ui.label("Folder:");
                            let r = ui.add(egui::TextEdit::singleline(&mut self.folder).hint_text("the design folder itself").desired_width(220.0));
                            if r.lost_focus() {
                                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    act = Some(("folder", self.folder.clone()));
                                } else {
                                    self.folder = current.clone();
                                }
                            }
                            if !user.is_empty()
                                && ui
                                    .small_button("Share folder\u{2026}")
                                    .on_hover_text("Who may see and change the designs in this folder")
                                    .clicked()
                            {
                                act = Some(("share_folder", String::new()));
                            }
                            if !current.is_empty() && ui.small_button("Up").on_hover_text("The folder above").clicked() {
                                act = Some(("folder", current.rsplit_once('/').map_or(String::new(), |(p, _)| p.to_string())));
                            }
                            for f in self.files.folders.borrow().iter() {
                                if ui.small_button(format!("{f}/")).clicked() {
                                    act = Some(("folder", if current.is_empty() { f.clone() } else { format!("{current}/{f}") }));
                                }
                            }
                        });
                    }
                    ui.separator();
                    if !self.files.ready.get() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(if remote { "Reading the server's folder…" } else { "Reading the browser's storage…" });
                        });
                        return;
                    }
                    let entries = self.files.entries.borrow().clone();
                    if entries.is_empty() {
                        ui.label(if remote {
                            "Designs you save appear here, in this folder on the server."
                        } else {
                            "Designs you save appear here. They stay in this browser."
                        });
                    }
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        egui::Grid::new("opfs_files").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                            for e in &entries {
                                match &mut self.renaming {
                                    Some((from, to)) if *from == e.name => {
                                        let r = ui.add(egui::TextEdit::singleline(to).desired_width(200.0));
                                        if std::mem::take(&mut self.rename_focus) {
                                            r.request_focus();
                                        }
                                        if r.lost_focus() {
                                            act = Some(if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                                ("rename", to.clone())
                                            } else {
                                                ("rename_cancel", String::new())
                                            });
                                        }
                                    }
                                    _ => {
                                        ui.label(&e.name);
                                    }
                                }
                                ui.label(format!("{:.1} KB", e.size as f64 / 1024.0));
                                ui.horizontal(|ui| {
                                    if ui.small_button("Open").clicked() {
                                        act = Some(("open", e.name.clone()));
                                    }
                                    let design = solidcraft_engine::io::is_design_path(&e.name);
                                    if !design && ui.small_button("Insert").on_hover_text("Into the open design (STEP, 3MF, STL)").clicked() {
                                        act = Some(("insert", e.name.clone()));
                                    }
                                    if ui.small_button("Rename").clicked() {
                                        self.renaming = Some((e.name.clone(), e.name.clone()));
                                        self.rename_focus = true;
                                    }
                                    if ui.small_button("Download").clicked() {
                                        act = Some(("download", e.name.clone()));
                                    }
                                    if ui.small_button("Delete").clicked() {
                                        act = Some(("delete", e.name.clone()));
                                    }
                                    if remote && ui.small_button("History").on_hover_text("Every version saved on the server").clicked() {
                                        act = Some(("history", e.name.clone()));
                                    }
                                    if remote
                                        && !user.is_empty()
                                        && ui.small_button("Share").on_hover_text("Who may see and change this design").clicked()
                                    {
                                        act = Some(("share", e.name.clone()));
                                    }
                                });
                                ui.end_row();
                            }
                        });
                    });
                    // What others shared with this user by name, wherever it is.
                    let shared = self.files.shared.borrow().clone();
                    if remote && !shared.is_empty() {
                        ui.separator();
                        egui::CollapsingHeader::new("Shared with me").default_open(true).show(ui, |ui| {
                            for s in &shared {
                                ui.horizontal(|ui| {
                                    let what = match (s.folder, s.path.is_empty()) {
                                        (true, true) => "the whole design folder".to_string(),
                                        (true, false) => format!("the folder {}", s.path),
                                        (false, _) => s.path.clone(),
                                    };
                                    ui.label(format!("{what} \u{b7} by {}{}", s.owner, if s.edit { "" } else { " (view only)" }));
                                    if ui.small_button("Go").on_hover_text("Open that folder in this panel").clicked() {
                                        let folder = if s.folder {
                                            s.path.clone()
                                        } else {
                                            s.path.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string())
                                        };
                                        act = Some(("folder", folder));
                                    }
                                });
                            }
                        });
                    }
                    // Who did what in this folder (the storage service's log).
                    let changes = self.files.changes.borrow().clone();
                    if remote && !changes.is_empty() {
                        ui.separator();
                        egui::CollapsingHeader::new("Recent changes").default_open(true).show(ui, |ui| {
                            for c in changes.iter().take(12) {
                                ui.label(format!("{} {} \u{b7} {}", c.author, c.message, files::ago(c.date))).on_hover_text(format!(
                                    "{} \u{b7} {}",
                                    files::when(c.date),
                                    c.path
                                ));
                            }
                        });
                    }
                });
            if !open {
                self.files.panel.set(false);
                self.renaming = None;
            }
            let Some((what, name)) = act else { return };
            let path = format!("{}{name}", files::OPFS);
            match what {
                "open" => {
                    self.app.open_path(&path);
                    self.files.panel.set(false);
                }
                "insert" => {
                    self.app.insert_path(&path);
                    self.files.panel.set(false);
                }
                "download" => match vfs::read(&path, solidcraft_engine::io::MAX_DESIGN_BYTES) {
                    Ok(b) => {
                        if let Err(e) = files::download(&name, &b) {
                            self.app.set_status(e, true);
                        }
                    }
                    Err(e) => self.app.set_status(e.to_string(), true),
                },
                "download_open" => {
                    let d = &self.app.session.doc;
                    let result = self
                        .app
                        .session
                        .design_container(solidcraft_engine::io::DesignCompression::Fast, false, false)
                        .map_err(|e| e.to_string())
                        .and_then(|bytes| files::download(&format!("{}.{}", d.name, solidcraft_engine::io::DESIGN_EXT), &bytes));
                    if let Err(e) = result {
                        self.app.set_status(e, true);
                    }
                }
                "delete" => {
                    files::delete(&self.files, &name);
                    self.app.home.recent.retain(|r| *r != path);
                }
                "history" => files::history(&self.files, &name),
                "share" => files::sharing(&self.files, files::Target::Design(name.clone())),
                "share_folder" => files::sharing(&self.files, files::Target::Folder),
                "users" => {
                    self.files.panel.set(false);
                    solidcraft_ui_egui::prefs::open_host_page(&mut self.app, 0);
                }
                "folder" => match files::set_remote_folder(&self.files, &name) {
                    Ok(()) => {
                        self.folder = files::remote_folder();
                        // The start page's recent designs follow the folder.
                        self.recents_synced = false;
                    }
                    Err(e) => self.app.set_status(e, true),
                },
                "rename_cancel" => self.renaming = None,
                "rename" => {
                    if let Some((from, _)) = self.renaming.take() {
                        match files::rename(&self.files, &from, &name) {
                            Ok(()) => {
                                // Recent designs follow the new name.
                                let (old, new) = (format!("{}{from}", files::OPFS), format!("{}{name}", files::OPFS));
                                for r in &mut self.app.home.recent {
                                    if *r == old {
                                        *r = new.clone();
                                    }
                                }
                            }
                            Err(e) => self.app.set_status(e, true),
                        }
                    }
                }
                _ => {}
            }
        }

        /// The versions of a server design (files::history): put one back, or bring it in as a copy.
        fn history_window(&mut self, ctx: &egui::Context) {
            let Some((name, versions)) = self.files.history.borrow().clone() else { return };
            let mut open = true;
            let mut act: Option<(&'static str, String, f64)> = None;
            let others: Vec<String> = self.files.entries.borrow().iter().map(|e| e.name.clone()).filter(|n| *n != name).collect();
            egui::Window::new(format!("History of {name}")).open(&mut open).collapsible(false).resizable(true).default_width(620.0).show(ctx, |ui| {
                // Work apart and bring it back: a branch is a copy whose edits can be replayed onto another design.
                ui.horizontal(|ui| {
                    ui.label("Branch as:");
                    if self.branch_name.is_empty() {
                        let (stem, ext) = name.rsplit_once('.').map(|(s, e)| (s.to_string(), format!(".{e}"))).unwrap_or((name.clone(), String::new()));
                        self.branch_name = format!("{stem} (branch){ext}");
                    }
                    ui.add(egui::TextEdit::singleline(&mut self.branch_name).desired_width(220.0));
                    if ui.small_button("Branch").on_hover_text("A copy to work on apart; merge it back here later").clicked() {
                        act = Some(("branch", self.branch_name.clone(), 0.0));
                    }
                    ui.separator();
                    ui.label("Merge into:");
                    egui::ComboBox::from_id_salt("merge_target").selected_text(if self.merge_target.is_empty() { "\u{2026}" } else { self.merge_target.as_str() }).show_ui(ui, |ui| {
                        for n in &others {
                            ui.selectable_value(&mut self.merge_target, n.clone(), n);
                        }
                    });
                    if !self.merge_target.is_empty() && ui.small_button("Merge").on_hover_text("Replays this design's edits onto that one (opens it, unsaved)").clicked() {
                        act = Some(("merge", self.merge_target.clone(), 0.0));
                    }
                });
                ui.separator();
                if versions.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Asking the server\u{2026}");
                    });
                    return;
                }
                ui.label("Every save, newest first. Restore makes a version the current one again (as a new version: nothing is lost); Copy brings it in as a separate design.");
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    egui::Grid::new("design_history").num_columns(4).spacing([12.0, 6.0]).show(ui, |ui| {
                        for (i, v) in versions.iter().enumerate() {
                            ui.label(format!("{} \u{b7} {}", files::ago(v.date), v.author)).on_hover_text(files::when(v.date));
                            ui.label(&v.message);
                            ui.label(format!("{:.1} KB", v.size as f64 / 1024.0));
                            ui.horizontal(|ui| {
                                if v.size > 0 && i > 0 && ui.small_button("Restore").clicked() {
                                    act = Some(("restore", v.hash.clone(), v.date));
                                }
                                if v.size > 0 && ui.small_button("Copy").clicked() {
                                    act = Some(("copy", v.hash.clone(), v.date));
                                }
                            });
                            ui.end_row();
                        }
                    });
                });
            });
            if !open {
                *self.files.history.borrow_mut() = None;
                self.branch_name.clear();
                self.merge_target.clear();
            }
            match act {
                Some(("restore", hash, _)) => files::restore(&self.files, &name, &hash),
                Some(("copy", hash, date)) => files::copy_version(&self.files, &name, &hash, date),
                Some(("branch", new_name, _)) => files::branch(&self.files, &name, &new_name),
                Some(("merge", target, _)) => {
                    self.sync.merge(&self.files, &name, &target);
                    *self.files.history.borrow_mut() = None;
                    self.files.panel.set(false);
                }
                _ => {}
            }
        }

        /// The Sharing window (files::sharing): who may view and who may change a design or the
        /// current folder; everyone, or only the people ticked.
        fn sharing_window(&mut self, ctx: &egui::Context) {
            let Some((target, label, access)) = self.files.sharing.borrow().clone() else { return };
            if self.share_for.as_ref() != Some(&target) {
                // Fresh choices for a new subject, from what the server said (once it has).
                let Some(a) = &access else { return };
                self.share_for = Some(target.clone());
                self.share_view = a.view.clone().map(|v| v.into_iter().collect());
                self.share_edit = a.edit.clone().map(|v| v.into_iter().collect());
            }
            let users: Vec<String> = self.files.users.borrow().as_ref().map(|u| u.list.iter().map(|(n, _)| n.clone()).collect()).unwrap_or_default();
            let mut open = true;
            let mut act: Option<&'static str> = None;
            egui::Window::new(format!("Sharing of {label}")).open(&mut open).collapsible(false).resizable(false).default_width(420.0).show(
                ctx,
                |ui| {
                    let Some(a) = &access else {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Asking the server\u{2026}");
                        });
                        return;
                    };
                    match (&a.owner, &a.inherited_from) {
                        (Some(owner), _) => ui.label(format!("Owner: {owner}")),
                        (None, Some(from)) => ui.label(format!(
                            "No sharing of its own: it follows {}",
                            if from == "/" { "the whole design folder".to_string() } else { format!("the folder {from}") }
                        )),
                        (None, None) => ui.label(if a.default_open {
                            "No sharing of its own: everyone may see and change it"
                        } else {
                            "No sharing of its own: only its creator and the admins"
                        }),
                    };
                    if !a.can_manage {
                        ui.label("Only its owner or an admin can change this.");
                    }
                    ui.add_enabled_ui(a.can_manage, |ui| {
                        for (what, chosen) in [("Can see it:", &mut self.share_view), ("Can change it:", &mut self.share_edit)] {
                            ui.separator();
                            ui.label(what);
                            ui.horizontal(|ui| {
                                if ui.radio(chosen.is_none(), "everyone").clicked() {
                                    *chosen = None;
                                }
                                if ui.radio(chosen.is_some(), "only these people (and the owner)").clicked() && chosen.is_none() {
                                    *chosen = Some(Default::default());
                                }
                            });
                            if let Some(set) = chosen {
                                ui.horizontal_wrapped(|ui| {
                                    for u in &users {
                                        let mut on = set.contains(u);
                                        if ui.checkbox(&mut on, u).changed() {
                                            if on {
                                                set.insert(u.clone());
                                            } else {
                                                set.remove(u);
                                            }
                                        }
                                    }
                                });
                            }
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Apply").clicked() {
                                act = Some("apply");
                            }
                            if a.owner.is_some() && ui.button("Follow the folder").on_hover_text("Drop this sharing of its own").clicked() {
                                act = Some("clear");
                            }
                        });
                    });
                },
            );
            if !open {
                *self.files.sharing.borrow_mut() = None;
                self.share_for = None;
            }
            match act {
                Some("apply") => {
                    let as_vec = |s: &Option<std::collections::BTreeSet<String>>| s.as_ref().map(|s| s.iter().cloned().collect::<Vec<_>>());
                    files::set_sharing(&self.files, target, as_vec(&self.share_view), as_vec(&self.share_edit));
                }
                Some("clear") => {
                    self.share_for = None;
                    files::clear_sharing(&self.files, target);
                }
                _ => {}
            }
        }

        /// Live collaboration (sync.rs): who else has this design open, and the locks: the active
        /// component can be held so only this user changes it.
        fn live_panel(&mut self, ctx: &egui::Context) {
            if !self.sync.is_shared() || self.app.home.open {
                return;
            }
            let me = files::remote_user();
            let others = self.sync.others();
            let locks = self.sync.locks();
            let active = self.app.session.active_component;
            let doc = self.app.session.doc.clone();
            let name_of = |c: u64| match c {
                0 => "the whole design".to_string(),
                c => doc.components.iter().find(|k| k.id == c).map_or_else(|| format!("component {c}"), |k| k.name.clone()),
            };
            let mut act: Option<(u64, bool)> = None;
            egui::Area::new(egui::Id::new("live_panel")).anchor(egui::Align2::RIGHT_TOP, [-16.0, 44.0]).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(if others.is_empty() {
                        "Shared on the server; nobody else here now".to_string()
                    } else {
                        format!("Also here: {}", others.join(", "))
                    });
                    match locks.iter().find(|(c, _)| *c == active).map(|(_, u)| u.as_str()) {
                        Some(u) if u == me => {
                            if ui.small_button(format!("Unlock {}", name_of(active))).clicked() {
                                act = Some((active, false));
                            }
                        }
                        Some(u) => {
                            ui.label(format!("{} is locked by {u}", name_of(active)));
                        }
                        None => {
                            let hint = "Only you can change it until you unlock it (or stop working for a few minutes)";
                            if ui.small_button(format!("Lock {}", name_of(active))).on_hover_text(hint).clicked() {
                                act = Some((active, true));
                            }
                        }
                    }
                    for (c, u) in locks.iter().filter(|(c, _)| *c != active) {
                        ui.label(format!("{} is locked by {u}", name_of(*c)));
                    }
                });
            });
            if let Some((component, take)) = act {
                self.sync.lock(&self.files, component, take);
            }
        }

        /// Offer the autosave an earlier session left.
        fn recovery_prompt(&mut self, ctx: &egui::Context) {
            let Some((name, path)) = self.files.recovery.borrow().clone() else { return };
            let mut answer = None;
            egui::Window::new("Recover unsaved work?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(
                ctx,
                |ui| {
                    ui.label(format!("“{name}” had unsaved changes when this page last closed."));
                    ui.horizontal(|ui| {
                        if ui.button("Recover").clicked() {
                            answer = Some(true);
                        }
                        if ui.button("Discard").clicked() {
                            answer = Some(false);
                        }
                    });
                },
            );
            let Some(recover) = answer else { return };
            *self.files.recovery.borrow_mut() = None;
            if recover {
                self.app.open_path(&files::recovery_path());
                // It belongs to its own file again (saving goes there); it is unsaved.
                self.app.session.path = path;
                self.app.session.doc_mut().name = name;
                self.app.session.mark_unsaved();
                self.app.home.open = false;
            }
            files::autosave(&self.files, None);
        }

        /// Fetch and install the CJK font when a CJK language is selected. If that fails, this
        /// session falls back to English (no tofu); the saved language preference is kept.
        fn tick_font(&mut self, ctx: &egui::Context) {
            let done = self.font_result.borrow_mut().take();
            if let Some(res) = done {
                let installed = match res {
                    Ok(bytes) => solidcraft_ui_egui::fonts::install_cjk_font(ctx, bytes),
                    Err(err) => {
                        log::warn!("failed to fetch CJK font from {JAPANESE_FONT_PATH}: {err}");
                        false
                    }
                };
                if installed {
                    log::info!("loaded web CJK font from {JAPANESE_FONT_PATH}");
                } else if solidcraft_ui_egui::fonts::needs_cjk_font(&self.app.preferences.language) {
                    log::warn!("CJK font unavailable; showing English for this session");
                }
                self.font.finished(installed, &mut self.app.preferences.language);
                ctx.request_repaint();
            }
            if self.font.tick(&mut self.app.preferences.language) {
                self.fetch_font(ctx);
            }
        }

        /// CJK text on screen with no CJK font yet (an English UI showing a Japanese name):
        /// fetch the font, so the text does not stay tofu (#720).
        fn watch_text_for_font(&mut self, ctx: &egui::Context) {
            if self.font.watching_text() && solidcraft_ui_egui::fonts::painted_cjk(ctx) && self.font.text_needs_font() {
                self.fetch_font(ctx);
            }
        }

        fn fetch_font(&self, ctx: &egui::Context) {
            let slot = self.font_result.clone();
            let ctx = ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let res = fetch_font_bytes(JAPANESE_FONT_PATH).await;
                *slot.borrow_mut() = Some(res);
                ctx.request_repaint();
            });
        }
    }

    impl eframe::App for Shell {
        fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            self.tick_font(ctx);
            self.app.logic(ctx);
            self.api.poll();
            self.space_mouse.logic(&mut self.app, ctx);
            self.files_tick(ctx);
            UNSAVED.with(|u| u.set(solidcraft_ui_egui::documents::any_dirty(&self.app)));
        }
        fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
            files::take_drops(&self.files, raw);
            // Before the app's own hook, which adds the control channel's events.
            self.space_mouse.raw_input_hook(ctx, self.app.preferences.space_mouse_enabled, raw);
            self.app.raw_input_hook(raw);
        }
        fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
            self.app.ui(ui);
            self.sync.tick(&mut self.app, &self.files);
            let ctx = ui.ctx().clone();
            self.file_panel(&ctx);
            self.history_window(&ctx);
            self.sharing_window(&ctx);
            self.live_panel(&ctx);
            self.recovery_prompt(&ctx);
            self.watch_text_for_font(&ctx);
        }
        fn save(&mut self, storage: &mut dyn eframe::Storage) {
            // Persist the user's own language, not a session-only English fallback.
            let saved = self.font.persisted_language(&self.app.preferences.language).to_owned();
            let live = std::mem::replace(&mut self.app.preferences.language, saved);
            storage.set_string(solidcraft_ui_egui::prefs::STORAGE_KEY, self.app.prefs());
            self.app.preferences.language = live;
        }
    }

    fn query() -> String {
        web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
    }

    /// Where `?webgl` is remembered. The installed app starts from its manifest's start URL, which
    /// has no query, so the choice has to outlive the page it was made on.
    const WEBGL_KEY: &str = "solidcraft.webgl";

    /// Render with WebGL2: asked for now (`?webgl`, remembered) or before (until `?webgpu`).
    fn webgl() -> bool {
        let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
        let q = query();
        if q.contains("webgl") {
            if let Some(s) = &storage {
                let _ = s.set_item(WEBGL_KEY, "1");
            }
            return true;
        }
        if q.contains("webgpu") {
            if let Some(s) = &storage {
                let _ = s.remove_item(WEBGL_KEY);
            }
            return false;
        }
        storage.and_then(|s| s.get_item(WEBGL_KEY).ok().flatten()).is_some()
    }

    /// Fetch font bytes asynchronously through the browser.
    async fn fetch_font_bytes(url: &str) -> Result<Vec<u8>, String> {
        let window = web_sys::window().ok_or("no window")?;
        let resp_val = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await.map_err(|e| format!("{e:?}"))?;
        let resp: web_sys::Response = resp_val.dyn_into().map_err(|_| "response cast failed")?;
        if !resp.ok() {
            return Err(format!("HTTP {}", resp.status()));
        }
        let ab_val = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?).await.map_err(|e| format!("{e:?}"))?;
        let uint8_arr = js_sys::Uint8Array::new(&ab_val);
        Ok(uint8_arr.to_vec())
    }

    pub fn start() {
        eframe::WebLogger::init(log::LevelFilter::Info).ok();
        wasm_bindgen_futures::spawn_local(async {
            let Some(window) = web_sys::window() else { return };
            ask_before_unload(&window);
            let Some(document) = window.document() else { return };
            let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
                log::error!("missing <canvas id=\"{CANVAS_ID}\">");
                return;
            };
            let mut options = eframe::WebOptions { depth_buffer: DEPTH_BITS, ..Default::default() };
            if webgl()
                && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
            {
                create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
            }
            let result = eframe::WebRunner::new()
                .start(
                    canvas,
                    options,
                    Box::new(move |cc| {
                        solidcraft_ui_egui::fonts::setup_fonts(&cc.egui_ctx);
                        let files = Rc::new(Files::default());
                        files::start(&files);
                        let panel = files.clone();
                        let (page_files, form) = (files.clone(), Rc::new(std::cell::RefCell::new(UsersForm::default())));
                        let home_files = files.clone();
                        let services = Services {
                            // Open shows the browser's files (and the computer's, from there).
                            pick_open: Some(Box::new(move |_: &[&str]| {
                                panel.panel.set(true);
                                Box::pin(std::future::ready(None))
                            })),
                            // Designs are kept in the browser; everything else is downloaded.
                            pick_save: Some(Box::new(|name: &str, exts: &[&str]| {
                                let design = exts.contains(&solidcraft_engine::io::DESIGN_EXT);
                                Box::pin(std::future::ready(Some(format!("{}{name}", if design { files::OPFS } else { files::DOWNLOADS }))))
                            })),
                            install_app: Some(crate::install::service(&cc.egui_ctx)),
                            // Preferences › Users: who can sign in to the server (files::users).
                            host_pages: vec![HostPage { title: "Users".into(), show: Box::new(move |ui| users_page(ui, &page_files, &form)) }],
                            home_sections: vec![Box::new(move |app, ui| shared_section(app, ui, &home_files))],
                            ..Default::default()
                        };
                        // Thumbnails of saves and start-page cards.
                        solidcraft_ui_egui::gpu::install_cpu_picture_renderer();
                        let (sender, receiver) = std::sync::mpsc::channel();
                        let mut app = SolveApp::new(Session::default(), services).with_control(receiver);
                        if let Some(p) = cc.storage.and_then(|s| solidcraft_ui_egui::prefs::stored(|k| s.get_string(k))) {
                            app.load_prefs(&p);
                        }
                        if let Some(rs) = &cc.wgpu_render_state {
                            // eframe's web painter has no stencil buffer.
                            app.set_wgpu(rs, DEPTH_BITS, 0, 1);
                        }
                        if query().contains("sample") {
                            if let Err(e) = app.session.run_script(&solidcraft_engine::sample::script()) {
                                app.set_status(format!("sample: {e}"), true);
                            }
                            app.session.doc_mut().name = "Sample Plate".into();
                            app.session.mark_saved();
                        }
                        if !query().contains("sample") {
                            // The empty design takes Units of new designs (#1205). The sample
                            // part is built in millimetres and is left that way.
                            solidcraft_ui_egui::prefs::apply_new_design(&mut app);
                            let rev = app.session.revision;
                            app.home.show(rev);
                        }
                        let api = Api::install(sender, cc.egui_ctx.clone()).map_err(|e| format!("SolidCraft browser API: {e:?}"))?;
                        Ok(Box::new(Shell {
                            app,
                            api,
                            files,
                            autosaved_at: f64::NEG_INFINITY,
                            autosaved: None,
                            renaming: None,
                            rename_focus: false,
                            folder: files::remote_folder(),
                            sync: crate::sync::Sync::new(),
                            branch_name: String::new(),
                            merge_target: String::new(),
                            share_for: None,
                            share_view: None,
                            share_edit: None,
                            recents_synced: false,
                            font: Default::default(),
                            font_result: Rc::new(std::cell::RefCell::new(None)),
                            space_mouse: SpaceMouse::default(),
                        }))
                    }),
                )
                .await;
            if let Some(el) = document.get_element_by_id(LOADING_ID) {
                match result {
                    Ok(()) => dismiss_loading(el),
                    Err(e) => {
                        el.set_inner_html(&format!("<p>SolidCraft failed to start: {e:?}</p><p>A browser with WebGPU or WebGL2 is required.</p>"))
                    }
                }
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {
    web::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("solidcraft-web only runs in the browser: build it with `trunk build --release` in apps/solidcraft-web");
}
