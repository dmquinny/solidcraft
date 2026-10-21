//! The first local appearance-library consumer, inside the existing Appearance panel.
//!
//! Self-hosting patch (not upstream): the panel also lists a library's physical materials and
//! assigns one to the selected bodies (density and default look, `library.apply`), and in the
//! browser, which has no file picker for libraries, it offers the ones the web app fetched from
//! the server's `Libraries` folder (apps/solidcraft-web/src/files.rs).
use crate::dialogs_appearance::selected_params;
use crate::{SolveApp, dialogs::row_label, dialogs_appearance::Ap, selection::SelInput, theme::Tokens};
use egui::RichText;
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
    row_label(ui, "Local library");
    ui.vertical(|ui| {
        #[cfg(not(target_arch = "wasm32"))]
        open_rows(app, ui, k);
        #[cfg(target_arch = "wasm32")]
        server_rows(app, ui, k);
        entry_rows(app, ui, k, inputs);
        ui.checkbox(&mut k.local.overwrite, crate::i18n::tr_label("Replace existing library file"));
        let save = ui.button(crate::i18n::tr_label("Export design library..."));
        crate::scenario::publish_handle("library:export", save.rect.center());
        if save.clicked() {
            if app.services.pick_save.is_some() {
                save_library(app, k.local.token.clone(), k.local.overwrite);
            } else if !k.local.path.is_empty() {
                export(app, k, k.local.path.clone(), k.local.overwrite);
            }
        }
        ui.label(
            RichText::new("Apply your appearance first. Exports assigned appearances and materials created in this design.")
                .small()
                .color(Tokens::get().text_dim),
        );
    });
    ui.end_row();
}

#[cfg(not(target_arch = "wasm32"))]
fn open_rows(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap) {
    let path = ui.add(egui::TextEdit::singleline(&mut k.local.path).hint_text("Library file").desired_width(200.0));
    crate::scenario::publish_handle("library:path", path.rect.center());
    if path.changed() {
        k.local.catalog = None;
        k.local.selected = None;
    }
    ui.horizontal(|ui| {
        if ui.button(crate::i18n::tr_label("Open library...")).clicked() {
            open_library(app, k.local.token.clone());
        }
        let load_button = ui.add_enabled(!k.local.path.is_empty(), egui::Button::new(crate::i18n::tr_label("Load library")));
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
        ui.label(
            RichText::new("Put .solidcraft-library.json files in the server's Libraries folder to use them here.")
                .small()
                .color(Tokens::get().text_dim),
        );
        return;
    }
    let label = |path: &str| path.strip_prefix(SERVER_LIBRARIES).unwrap_or(path).trim_end_matches(".solidcraft-library.json").to_string();
    let mut chosen = if found.len() == 1 && k.local.path.is_empty() { found.first().cloned() } else { None };
    let current = if k.local.path.is_empty() { "Choose a library".to_string() } else { label(&k.local.path) };
    egui::ComboBox::from_id_salt("server_libraries").selected_text(current).show_ui(ui, |ui| {
        for path in &found {
            if ui.selectable_label(k.local.path == *path, label(path)).clicked() {
                chosen = Some(path.clone());
            }
        }
    });
    if let Some(path) = chosen.filter(|path| *path != k.local.path) {
        k.local.path = path;
        k.local.catalog = None;
        k.local.selected = None;
        k.local.search.clear();
        k.note = Some(load(app, &mut k.local).unwrap_or_else(|error| error));
    }
}

/// The open library's entries (appearances, then physical materials) and the button that
/// applies the selected one.
fn entry_rows(app: &mut SolveApp, ui: &mut egui::Ui, k: &mut Ap, inputs: &[SelInput]) {
    if let Some(catalog) = &k.local.catalog {
        ui.label(RichText::new(&catalog.name).strong());
        let search = ui.add(egui::TextEdit::singleline(&mut k.local.search).hint_text("Search this library").desired_width(200.0));
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
            ui.label("No matching appearance presets or materials.");
        }
        let row_height = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical().id_salt("local_library_entries").max_height(120.0).show_rows(ui, row_height, shown.len(), |ui, range| {
            for index in range {
                let Some(entry) = shown.get(index) else { continue };
                let (name, color, detail) = match &entry.definition {
                    Definition::Appearance(look) => (look.name.as_str(), look.color, format!("Opacity {:.0}%", look.opacity * 100.0)),
                    Definition::Material(material) => (
                        material.name.as_str(),
                        material.appearance.as_ref().map_or([150, 150, 150], |look| look.color),
                        format!("Physical material, {} g/cm³", material.density_g_cm3),
                    ),
                };
                ui.horizontal(|ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                    let [r, g, b] = color;
                    ui.colored_label(egui::Color32::from_rgb(r, g, b), "■");
                    let button = ui.selectable_label(k.local.selected.as_deref() == Some(entry.id.as_str()), name).on_hover_text(format!(
                        "{detail} · {} · {}",
                        entry.provenance.license,
                        entry.provenance.source_url.as_deref().unwrap_or("Local definition")
                    ));
                    crate::scenario::publish_handle(&format!("library:entry:{index}"), button.rect.center());
                    if button.clicked() {
                        k.local.selected = Some(entry.id.clone());
                    }
                    if let Definition::Material(material) = &entry.definition {
                        ui.label(RichText::new(format!("{} g/cm³", material.density_g_cm3)).small().color(Tokens::get().text_dim));
                    }
                });
            }
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
    let apply = ui.add_enabled(entry.is_some() && selected.is_some(), egui::Button::new(crate::i18n::tr_label("Apply library appearance")));
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
                    k.pbr = None;
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
    let assign = ui
        .add_enabled(!bodies.is_empty(), egui::Button::new(crate::i18n::tr_label("Assign library material")))
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
