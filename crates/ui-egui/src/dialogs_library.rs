//! The first local appearance-library consumer, inside the existing Appearance panel.
//!
//! Self-hosting patch (not upstream): the panel also lists a library's physical materials and
//! assigns one to the selected bodies (density and default look, `library.apply`), and in the
//! browser, which has no file picker for libraries, it offers the ones the web app fetched from
//! the server's `Libraries` folder (apps/solidcraft-web/src/files.rs).
use crate::dialogs_appearance::selected_params;
use crate::{SolveApp, dialogs_appearance::Ap, selection::SelInput, ui_kit};
use serde_json::{Value, json};
use solidcraft_engine::io::catalog::{Catalog, Definition, Entry};
use std::sync::Arc;

/// Where the web app puts the libraries it fetched from the server's `Libraries` folder
/// (`LIBRARIES` in apps/solidcraft-web/src/files.rs).
#[cfg(target_arch = "wasm32")]
pub const SERVER_LIBRARIES: &str = "/libraries/";

#[derive(Clone, Debug, Default)]
pub struct LocalLibrary {
    /// A delayed picker may update only the editor that requested it.
    token: Arc<()>,
    pub search: String,
    pub path: String,
    pub catalog: Option<Catalog>,
    pub selected: Option<String>,
    pub overwrite: bool,
    /// The name a library is saved to the server under (the browser).
    pub save_name: String,
}

/// The looks of the loaded libraries, for the Appearance window's swatches (a self-hosting
/// patch): every appearance and every material's default look, with its pattern and surface
/// response, in the category its name starts with ("Wood - Pine, Knotty" is Wood). In the
/// browser, every library the web host fetched from the server's Libraries folder (decoded again
/// only when they change); everywhere, the library open in this window.
pub fn swatch_looks(local: &LocalLibrary) -> Vec<crate::dialogs_appearance::Look> {
    #[cfg(target_arch = "wasm32")]
    let mut out = server_looks();
    #[cfg(not(target_arch = "wasm32"))]
    let mut out = Vec::new();
    if let Some(catalog) = &local.catalog {
        for look in catalog_looks(catalog) {
            if !out.iter().any(|l: &crate::dialogs_appearance::Look| l.name.eq_ignore_ascii_case(&look.name)) {
                out.push(look);
            }
        }
    }
    out
}

fn catalog_looks(catalog: &Catalog) -> Vec<crate::dialogs_appearance::Look> {
    catalog
        .entries
        .iter()
        .filter_map(|entry| match &entry.definition {
            Definition::Appearance(look) => Some(look),
            Definition::Material(material) => material.appearance.as_ref(),
        })
        .map(|look| crate::dialogs_appearance::Look {
            name: look.name.clone(),
            category: look.name.split(" - ").next().map(str::trim).filter(|c| !c.is_empty() && *c != look.name).unwrap_or("Library").to_string(),
            color: look.color,
            opacity: look.opacity,
            pbr: look.pbr,
            pattern: look.pattern,
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// The server libraries' looks, and the files (path, size) they were read from.
    static SERVER_LOOKS: std::cell::RefCell<(Vec<(String, u64)>, Vec<crate::dialogs_appearance::Look>)> = Default::default();
}

#[cfg(target_arch = "wasm32")]
fn server_looks() -> Vec<crate::dialogs_appearance::Look> {
    let mut files = solidcraft_engine::io::vfs::list(SERVER_LIBRARIES);
    files.sort();
    SERVER_LOOKS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.0 != files {
            let mut looks: Vec<crate::dialogs_appearance::Look> = Vec::new();
            for (path, _) in &files {
                let Ok(catalog) = Catalog::read(path) else { continue };
                for look in catalog_looks(&catalog) {
                    if !looks.iter().any(|l| l.name.eq_ignore_ascii_case(&look.name)) {
                        looks.push(look);
                    }
                }
            }
            *cache = (files, looks);
        }
        cache.1.clone()
    })
}

fn display_order(mut catalog: Catalog) -> Catalog {
    catalog.entries.sort_by(|a, b| a.definition.name().cmp(b.definition.name()).then_with(|| a.id.cmp(&b.id)));
    catalog
}

fn editor<'a>(app: &'a mut SolveApp, token: &Arc<()>) -> Option<&'a mut Ap> {
    let crate::dialogs::Kind::Appearance(k) = &mut app.dialog.as_mut()?.kind else { return None };
    Arc::ptr_eq(&k.local.token, token).then_some(k)
}

fn save_library(app: &mut SolveApp, token: Arc<()>, overwrite: bool) {
    let origin = app.session.doc.clone();
    app.pick_save("my-library.solidcraft-library.json", &["json"], move |app, path| {
        let Some(path) = path else { return };
        if !Arc::ptr_eq(&origin, &app.session.doc) {
            app.set_status("The library export document changed while choosing its destination", true);
            return;
        }
        match app.run("library.export", json!({"path":path,"overwrite":overwrite})) {
            Ok(result) => {
                let parsed = result.get("catalog").cloned().and_then(|v| serde_json::from_value::<Catalog>(v).ok()).map(display_order);
                let count = parsed.as_ref().map_or(0, |c| c.entries.len());
                if let Some(k) = editor(app, &token) {
                    k.local.path = path;
                    k.local.catalog = parsed;
                    k.local.selected = None;
                    k.local.search.clear();
                    k.note = Some(format!("Saved {count} reusable definitions. The design is unchanged."));
                }
            }
            Err(error) => {
                if let Some(k) = editor(app, &token) {
                    k.note = Some(error);
                }
            }
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn open_library(app: &mut SolveApp, token: Arc<()>) {
    app.pick_open(move |app, path| {
        let Some(path) = path else { return };
        let Some(k) = editor(app, &token) else { return };
        let mut local = k.local.clone();
        local.path = path;
        local.catalog = None;
        local.selected = None;
        let note = load(app, &mut local).unwrap_or_else(|error| error);
        if let Some(k) = editor(app, &token) {
            k.local = local;
            k.note = Some(note);
        }
    });
}

/// Exports the design's library to `path` and shows it as the local library.
fn export(app: &mut SolveApp, k: &mut Ap, path: String, overwrite: bool) {
    k.note = Some(match app.run("library.export", json!({"path":path,"overwrite":overwrite})) {
        Ok(result) => {
            let parsed = result.get("catalog").cloned().and_then(|v| serde_json::from_value::<Catalog>(v).ok()).map(display_order);
            let count = parsed.as_ref().map_or(0, |c| c.entries.len());
            k.local.path = path;
            k.local.catalog = parsed;
            k.local.selected = None;
            k.local.search.clear();
            format!("Saved {count} reusable definitions. The design is unchanged.")
        }
        Err(error) => error,
    });
}

fn load(app: &mut SolveApp, library: &mut LocalLibrary) -> Result<String, String> {
    let result = app.run("library.inspect", json!({"path":library.path}))?;
    let catalog: Catalog = serde_json::from_value(result.get("catalog").cloned().ok_or("Library response is missing")?).map_err(|e| e.to_string())?;
    let note = format!("Opened {} ({} entries)", catalog.name, catalog.entries.len());
    library.catalog = Some(display_order(catalog));
    library.selected = None;
    library.search.clear();
    Ok(note)
}

pub fn rows(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap, inputs: &[SelInput]) {
    // Folded (a self-hosting patch): its looks are already among the window's, so this is for
    // opening and saving library files. A new id, so it starts folded where it was open.
    ui_kit::section(ui, "local_library_files", "Local library", false, |ui| {
        #[cfg(not(target_arch = "wasm32"))]
        open_rows(app, ui, k);
        #[cfg(target_arch = "wasm32")]
        server_rows(app, ui, k);
        entry_rows(app, ui, k, inputs);
        ui_kit::checkbox(ui, crate::i18n::tr_label("Replace existing library file"), &mut k.local.overwrite);
        let save = ui_kit::button(ui, crate::i18n::tr_label("Export design library..."), ui_kit::Button::Normal);
        crate::scenario::publish_handle("library:export", save.rect.center());
        if save.clicked() {
            if app.services.pick_save.is_some() {
                save_library(app, k.local.token.clone(), k.local.overwrite);
            } else if !k.local.path.is_empty() {
                export(app, k, k.local.path.clone(), k.local.overwrite);
            }
        }
        ui_kit::text(
            ui,
            crate::i18n::tr_label("Apply your appearance first. Exports assigned appearances and materials created in this design."),
            ui_kit::Text::Note("library_export"),
        );
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn open_rows(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap) {
    let path = ui_kit::row(ui, "Library file", |ui| {
        let id = ui.make_persistent_id("library_path");
        ui_kit::cell_editor(ui, id, &mut k.local.path, "Library file", false)
    });
    crate::scenario::publish_handle("library:path", path.rect.center());
    if path.changed() {
        k.local.catalog = None;
        k.local.selected = None;
    }
    ui.horizontal(|ui| {
        if ui_kit::button(ui, crate::i18n::tr_label("Open library..."), ui_kit::Button::Normal).clicked() {
            open_library(app, k.local.token.clone());
        }
        let load_button = ui_kit::button(ui, "Load library", ui_kit::Button::Enabled(!k.local.path.is_empty()));
        crate::scenario::publish_handle("library:load", load_button.rect.center());
        if load_button.clicked() {
            k.local.catalog = None;
            k.local.selected = None;
            k.note = Some(load(app, &mut k.local).unwrap_or_else(|error| error));
        }
    });
}

/// The browser's libraries: those the web app fetched from the server's `Libraries` folder, in
/// a list that also says which one is open. A single one opens straight away.
#[cfg(target_arch = "wasm32")]
fn server_rows(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap) {
    let mut found: Vec<String> = solidcraft_engine::io::vfs::list(SERVER_LIBRARIES).into_iter().map(|(path, _)| path).collect();
    found.sort_by_key(|path| path.to_lowercase());
    if found.is_empty() {
        ui_kit::text(
            ui,
            "Put .solidcraft-library.json files in the server's Libraries folder to use them here, or save this design's looks there below.",
            ui_kit::Text::Note("library_platform"),
        );
        save_rows(app, ui, k);
        return;
    }
    let mut options = vec!["Choose a library"];
    let labels: Vec<&str> =
        found.iter().map(|path| path.strip_prefix(SERVER_LIBRARIES).unwrap_or(path).trim_end_matches(".solidcraft-library.json")).collect();
    options.extend(labels);
    let open = found.iter().position(|path| *path == k.local.path).map_or(0, |i| i + 1);
    let mut index = open;
    ui_kit::row(ui, "Library", |ui| ui_kit::dropdown(ui, "server_library", &mut index, &options));
    // Nothing open yet: the first library opens by itself. Otherwise a pick opens that one.
    let chosen = if k.local.path.is_empty() {
        found.first().cloned()
    } else if index != open && index > 0 {
        found.get(index - 1).cloned()
    } else {
        None
    };
    if let Some(path) = chosen {
        k.local.path = path;
        k.local.catalog = None;
        k.local.selected = None;
        k.local.search.clear();
        k.note = Some(load(app, &mut k.local).unwrap_or_else(|error| error));
    }
    save_rows(app, ui, k);
}

/// Save this design's appearances and materials (their patterns too) as a library in the
/// server's Libraries folder, where every browser finds it: written under `SERVER_LIBRARIES`,
/// which the web host uploads (or downloads, without server storage). Make your own looks first
/// (Appearance, or a physical material with a default look), then name the library and save.
#[cfg(target_arch = "wasm32")]
fn save_rows(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap) {
    let name = ui_kit::row(ui, "Save as library", |ui| {
        let id = ui.make_persistent_id("library_save_name");
        ui_kit::cell_editor(ui, id, &mut k.local.save_name, "My stones", false)
    });
    crate::scenario::publish_handle("library:save_name", name.rect.center());
    let title = k.local.save_name.trim().to_string();
    let save = ui_kit::button(ui, "Save to the server", ui_kit::Button::Enabled(!title.is_empty()));
    crate::scenario::publish_handle("library:save_server", save.rect.center());
    if !save.clicked() {
        return;
    }
    if app.session.doc.appearances.is_empty() && app.session.doc.user_materials.is_empty() {
        k.note = Some("Give bodies an appearance or make a physical material first: those are what a library keeps".into());
        return;
    }
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(48)
        .collect();
    let slug = if slug.is_empty() { "library".to_string() } else { slug };
    let path = format!("{SERVER_LIBRARIES}{slug}.solidcraft-library.json");
    let params =
        json!({"path": path, "id": format!("user.{slug}"), "name": title.chars().take(128).collect::<String>(), "overwrite": k.local.overwrite});
    k.note = Some(match app.run("library.export", params) {
        Ok(result) => {
            k.local.path = path;
            match load(app, &mut k.local) {
                Ok(_) => {
                    format!("{title}: {} definitions saved as {slug}.solidcraft-library.json in the server's Libraries folder", result["entries"])
                }
                Err(error) => error,
            }
        }
        Err(error) => error,
    });
}

/// The open library's entries (appearances, then physical materials) and the button that
/// applies the selected one.
fn entry_rows(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap, inputs: &[SelInput]) {
    if let Some(catalog) = &k.local.catalog {
        ui_kit::text(ui, &catalog.name, ui_kit::Text::Report("library_name", false));
        let search = ui_kit::row(ui, "Search this library", |ui| {
            let id = ui.make_persistent_id("library_search");
            ui_kit::search_field(ui, id, &mut k.local.search, "Search this library")
        });
        crate::scenario::publish_handle("library:search", search.rect.center());
        if search.changed() {
            k.local.selected = None;
        }
        let query = k.local.search.trim().to_lowercase();
        // A material is found by its own name or its look's ("Wood" finds every wood).
        let found = |entry: &&Entry| {
            let look = match &entry.definition {
                Definition::Appearance(look) => Some(look),
                Definition::Material(material) => material.appearance.as_ref(),
            };
            entry.definition.name().to_lowercase().contains(&query) || look.is_some_and(|look| look.name.to_lowercase().contains(&query))
        };
        // Appearances first: they keep the positions their scenario handles have always named.
        let shown: Vec<&Entry> = catalog
            .entries
            .iter()
            .filter(|entry| matches!(&entry.definition, Definition::Appearance(_)))
            .filter(found)
            .chain(catalog.entries.iter().filter(|entry| matches!(&entry.definition, Definition::Material(_))).filter(found))
            .collect();
        if shown.is_empty() {
            ui_kit::text(ui, crate::i18n::tr_label("No matching appearance presets."), ui_kit::Text::Note("library_empty"));
        }
        let row_height = ui_kit::row_height();
        ui_kit::list_frame(ui, "local_library_entries", |ui| {
            egui::ScrollArea::vertical().id_salt("local_library_entries").max_height(row_height * 5.0).show_rows(
                ui,
                row_height,
                shown.len(),
                |ui, range| {
                    for index in range {
                        let Some(entry) = shown.get(index) else { continue };
                        let (name, color, detail) = match &entry.definition {
                            Definition::Appearance(look) => (look.name.clone(), look.color, format!("Opacity {:.0}%", look.opacity * 100.0)),
                            Definition::Material(material) => (
                                format!("{} · {} g/cm³", material.name, material.density_g_cm3),
                                material.appearance.as_ref().map_or([150, 150, 150], |look| look.color),
                                format!("Physical material, {} g/cm³", material.density_g_cm3),
                            ),
                        };
                        ui.horizontal(|ui| {
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                            let [r, g, b] = color;
                            ui_kit::color_thumbnail(ui, ui.id().with((&entry.id, "sample")), egui::Color32::from_rgb(r, g, b), &name, ());
                            let button = ui_kit::list_picker(ui, &entry.id, &name, k.local.selected.as_deref() == Some(entry.id.as_str()))
                                .on_hover_text(format!(
                                    "{detail} · {} · {}",
                                    entry.provenance.license,
                                    entry.provenance.source_url.as_deref().unwrap_or("Local definition")
                                ));
                            crate::scenario::publish_handle(&format!("library:entry:{index}"), button.rect.center());
                            if button.clicked() {
                                k.local.selected = Some(entry.id.clone());
                            }
                        });
                    }
                },
            );
        });
    }
    let entry =
        k.local.catalog.as_ref().and_then(|catalog| catalog.entries.iter().find(|entry| Some(&entry.id) == k.local.selected.as_ref())).cloned();
    let picked = inputs.first().map(|input| input.items.as_slice()).unwrap_or(&[]);
    let selected = selected_params(k, picked);
    if let Some(entry) = entry.as_ref().filter(|entry| matches!(&entry.definition, Definition::Material(_))) {
        material_apply(app, ui, k, entry, selected.as_ref());
        return;
    }
    let entry = entry.filter(|entry| matches!(&entry.definition, Definition::Appearance(_)));
    let apply = ui_kit::button(ui, "Apply library appearance", ui_kit::Button::Enabled(entry.is_some() && selected.is_some()));
    crate::scenario::publish_handle("library:apply", apply.rect.center());
    if apply.clicked()
        && let (Some(entry), Some(selected)) = (entry, selected)
    {
        let mut targets = serde_json::Map::new();
        for key in ["bodies", "faces", "components"] {
            if let Some(value) = selected.get(key) {
                targets.insert(key.into(), value.clone());
            }
        }
        k.note = Some(match app.run("library.apply", json!({"path":k.local.path,"entry":entry.id,"revision":entry.revision,"targets":targets})) {
            Ok(_) => {
                if let Definition::Appearance(look) = entry.definition {
                    k.color = look.color;
                    k.pbr = look.pbr;
                    k.pattern = look.pattern;
                    k.opacity = look.opacity;
                    k.look = Some(look.name.clone());
                    k.remove_override = false;
                    k.applied = selected_params(k, picked);
                    format!("{} copied into this design", look.name)
                } else {
                    "This panel applies appearance presets only".into()
                }
            }
            Err(error) => error,
        });
    }
}

/// Assign a library's physical material to the selected bodies: `library.apply` copies it into
/// the design (density and default look) and assigns it, in one undo step.
fn material_apply(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap, entry: &Entry, selected: Option<&Value>) {
    let Definition::Material(material) = &entry.definition else { return };
    // A physical material belongs to a whole body, never to a face or a component.
    let bodies: Vec<String> = selected
        .filter(|params| params.get("faces").is_none() && params.get("components").is_none())
        .and_then(|params| params.get("bodies"))
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(|name| name.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let assign = ui_kit::button(ui, "Assign library material", ui_kit::Button::Enabled(!bodies.is_empty()))
        .on_disabled_hover_text("Select whole bodies: a physical material goes on a body, not on a face or a component");
    crate::scenario::publish_handle("library:assign", assign.rect.center());
    if !assign.clicked() {
        return;
    }
    k.note =
        Some(match app.run("library.apply", json!({"path":k.local.path,"entry":entry.id,"revision":entry.revision,"targets":{"bodies":bodies}})) {
            Ok(_) => {
                // OK then closes without painting the editor's colour over the material's look.
                k.applied = selected.cloned();
                let covered = bodies.iter().any(|body| app.session.doc.appearances.bodies.contains_key(body.as_str()));
                let mut note = format!("{} assigned ({} g/cm³)", material.name, material.density_g_cm3);
                if covered {
                    note.push_str(". A body's own appearance still shows over the material's colour; remove that override to see it");
                }
                note
            }
            Err(error) => error,
        });
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "dialogs_library_tests.rs"]
mod tests;
